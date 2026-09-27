use std::path::{Path, PathBuf};

mod git_read;
mod git_write;
mod landing;
mod layout;
mod member;
mod merge;
mod review;
mod verifier;
mod workspace_gc;

#[cfg(test)]
pub(crate) use git_read::git_metadata_dirs_from_stdout;
use git_read::{git_read_command, GIT_CONFIG_SUBCOMMAND};
pub(crate) use git_read::{
    git_read_output, git_read_stdout_checked, head_tracked_entries, head_tracked_subset,
    resolve_git_author_identity, resolve_git_metadata_dirs,
};
#[cfg(test)]
use git_read::{read_head_entry, HeadEntry, HARDENED_GIT_READ_PREFIX};
#[cfg(test)]
use git_write::{
    build_add_argv, build_commit_argv, configure_git_write_environment, empty_git_home,
    local_git_filter_drivers, HARDENED_GIT_WRITE_PREFIX,
};
pub(crate) use git_write::{
    reject_ignored_exact_paths, run_sandboxed_git_commit, validate_sandboxed_commit_inputs,
};
pub(crate) use landing::{
    artifact_diff_text, changed_paths_between, changed_paths_between_no_renames,
    checkpoint_path_dirty_states, is_ancestor, landed_review, landing_stats, numstat_files_between,
    protected_landing_paths, worktree_is_dirty,
};
pub use landing::{run_numstat, synthesize_hard_fields};
pub(crate) use layout::{base_repo_for_local_session, default_root, default_sessions_root};
use layout::{
    canonical_managed_worktree, ensure_worktree_for_default_in, ensure_worktree_in, home_dir,
};
pub use layout::{
    cleanup_continuation_workspace, derive_continuation_workspace, journals_dir,
    local_sessions_root, logs_dir, safe_id,
};

#[cfg(test)]
use member::add_member_worktree;
pub use member::{cleanup_member_workspace, ensure_member_workspace};
pub use merge::{
    finalize_session_before_cleanup, merge_artifact_to_session_head, merge_artifact_to_staging,
    MergeOutcome, SessionMergeOutcome,
};
use merge::{git_symbolic_head, session_integration_guard};
#[cfg(test)]
use review::{
    append_no_index_patch, append_untracked_review_files, review_scoped_with_budget,
    review_working_tree_at_with_no_index,
};
pub(crate) use review::{
    apply_staging_ff_only, combine_reviews, count_unattributed_dirty, count_unique_files,
    delete_staging_branch, review_scoped,
};
use review::{review_in, review_working_tree_at};
#[cfg(all(test, target_os = "macos"))]
use verifier::seatbelt_verifier_profile;
use verifier::TempVerifyWorktree;
#[cfg(all(test, target_os = "macos"))]
use verifier::{
    build_verifier_sandbox_command, contains_word, sandbox_denied_signature,
    truncate_verifier_output_head_tail, VERIFIER_OUTPUT_HEAD_BYTES, VERIFIER_OUTPUT_TAIL_BYTES,
};
pub use verifier::{run_verifier, run_verifier_in_place, VerifyResult};
use workspace_gc::git_ref_exists;
pub use workspace_gc::{
    gc_trashed_session_branch, move_restored_session_branch_back_to_trash,
    release_session_workspace, restore_trashed_session_branch, trash_session_workspace,
};
pub(crate) use workspace_gc::{
    move_to_unique_trash, session_wt_path, trash_clean_orphan_workspace,
    trash_deleted_session_head_without_workspace, worktree_belongs_to_repo,
};
#[cfg(test)]
use workspace_gc::{release_or_trash_in, BranchDisposition};

/// 事后审结果：agent 这批改了什么（committed + uncommitted + untracked，相对 fork 点）。
#[derive(Clone, Debug, serde::Serialize)]
pub struct ReviewFile {
    pub path: String,
    pub undoable: bool,
}

#[derive(serde::Serialize)]
pub struct Review {
    pub has_changes: bool,
    pub stat: String,
    pub patch: String,
    /// Provide a structured file count for the badge so the frontend need not parse stat text.
    pub files_changed: u64,
    /// Review 中逐文件的能力边界：只有 checkpoint 账本记过 preimage 才可撤销。
    pub files: Vec<ReviewFile>,
    /// 工作区里不属于当前会话归因集合的脏文件数。
    #[serde(default)]
    pub other_dirty_count: u64,
    /// false 表示目录不是带 HEAD 的 git 工作树，Review 只能优雅降级为空态。
    pub diff_available: bool,
    /// 状态摘要用（commit 3）：已提交段落覆盖的不重复文件数。默认 0——只有走归因求和主路径
    /// 才会填真值；折入/legacy 分支各自按自己的语义显式赋值，绝不留一个会说谎的默认态。
    #[serde(default)]
    pub committed_files_changed: u64,
    /// 状态摘要用（commit 3）：当前未提交（`git diff HEAD`）覆盖的不重复文件数。
    #[serde(default)]
    pub uncommitted_files_changed: u64,
}

impl Review {
    fn empty() -> Self {
        Review {
            has_changes: false,
            stat: String::new(),
            patch: String::new(),
            files_changed: 0,
            files: Vec::new(),
            other_dirty_count: 0,
            diff_available: true,
            committed_files_changed: 0,
            uncommitted_files_changed: 0,
        }
    }

    pub(crate) fn unavailable() -> Self {
        Review {
            diff_available: false,
            ..Review::empty()
        }
    }

    pub(crate) fn mark_undoable_paths(&mut self, project: &Path, checkpoint_paths: &[PathBuf]) {
        let case_insensitive = filesystem_is_case_insensitive(project);
        let checkpoint_paths = checkpoint_paths
            .iter()
            .filter_map(|path| normalize_project_relative_path(project, path, case_insensitive))
            .collect::<std::collections::HashSet<_>>();
        for file in &mut self.files {
            file.undoable =
                normalize_project_relative_path(project, Path::new(&file.path), case_insensitive)
                    .is_some_and(|path| checkpoint_paths.contains(&path));
        }
    }
}

/// Commit 2 用：把一条 checkpoint 记录的路径（可能是绝对路径）归一成跟 git 输出同口径的
/// 项目相对路径 key（含大小写敏感性判定），方便跟 `changed_paths_between_no_renames` 之类
/// 返回的相对路径集合做匹配。
pub(crate) fn normalize_checkpoint_path_key(project: &Path, path: &Path) -> Option<String> {
    let case_insensitive = filesystem_is_case_insensitive(project);
    normalize_project_relative_path(project, path, case_insensitive)
}

fn normalize_project_relative_path(
    project: &Path,
    path: &Path,
    case_insensitive: bool,
) -> Option<String> {
    let canonical_project =
        std::fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf());
    let relative = if path.is_absolute() {
        path.strip_prefix(project)
            .or_else(|_| path.strip_prefix(&canonical_project))
            .ok()?
    } else {
        path
    };
    let mut normalized = PathBuf::new();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(part) => normalized.push(part),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => return None,
        }
    }
    let normalized = normalized.to_string_lossy().replace('\\', "/");
    if case_insensitive {
        Some(normalized.to_ascii_lowercase())
    } else {
        Some(normalized)
    }
}

#[cfg(target_os = "macos")]
fn filesystem_is_case_insensitive(project: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;

    let Ok(path) = std::ffi::CString::new(project.as_os_str().as_bytes()) else {
        return false;
    };
    // pathconf is a read-only query of the project's containing filesystem. Errors fail closed to
    // case-sensitive comparison instead of broadening checkpoint capability.
    unsafe { libc::pathconf(path.as_ptr(), libc::_PC_CASE_SENSITIVE) == 0 }
}

#[cfg(not(target_os = "macos"))]
fn filesystem_is_case_insensitive(_project: &Path) -> bool {
    false
}

fn git_stdout(dir: &Path, args: &[&str]) -> Result<String, String> {
    let o = git_read_output(dir, args).map_err(|e| {
        crate::ui_msg::al_err(
            "wt.git.spawnFailed",
            &[("cmd", format!("{args:?}")), ("detail", e.to_string())],
        )
    })?;
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

pub(crate) fn git_checked_stdout(dir: &Path, args: &[&str]) -> Result<String, String> {
    let o = git_read_output(dir, args).map_err(|e| {
        crate::ui_msg::al_err(
            "wt.git.spawnFailed",
            &[("cmd", format!("{args:?}")), ("detail", e.to_string())],
        )
    })?;
    if !o.status.success() {
        return Err(crate::ui_msg::al_err(
            "wt.git.commandFailed",
            &[
                ("cmd", format!("{args:?}")),
                ("stderr", String::from_utf8_lossy(&o.stderr).to_string()),
            ],
        ));
    }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

fn run_git(dir: &Path, args: &[&str]) -> Result<(), String> {
    let o = crate::proc::command("git")
        .current_dir(dir)
        .args(args)
        .output()
        .map_err(|e| {
            crate::ui_msg::al_err(
                "wt.git.spawnFailed",
                &[("cmd", format!("{args:?}")), ("detail", e.to_string())],
            )
        })?;
    if !o.status.success() {
        return Err(crate::ui_msg::al_err(
            "wt.git.commandFailed",
            &[
                ("cmd", format!("{args:?}")),
                ("stderr", String::from_utf8_lossy(&o.stderr).to_string()),
            ],
        ));
    }
    Ok(())
}

/// status-checked `git rev-parse HEAD`：进程级/业务级失败都冒泡 Err（不像 git_stdout 吞退出码）。
/// pub(crate)：git-only review / landing paths 共用此实现。
pub(crate) fn rev_parse_head(dir: &Path) -> Result<String, String> {
    let o = git_read_output(dir, &["rev-parse", "HEAD"]).map_err(|e| {
        crate::ui_msg::al_err("wt.git.revParseSpawnFailed", &[("detail", e.to_string())])
    })?;
    if !o.status.success() {
        return Err(crate::ui_msg::al_err(
            "wt.git.revParseFailed",
            &[("stderr", String::from_utf8_lossy(&o.stderr).to_string())],
        ));
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn session_status_stdout(dir: &Path, phase: &str) -> Result<String, String> {
    let args = ["status", "--porcelain"];
    let out = git_read_output(dir, &args).map_err(|e| {
        crate::ui_msg::al_err(
            "wt.git.sessionStatusSpawnFailed",
            &[("phase", phase.to_string()), ("detail", e.to_string())],
        )
    })?;
    if !out.status.success() {
        return Err(crate::ui_msg::al_err(
            "wt.git.sessionStatusFailed",
            &[
                ("phase", phase.to_string()),
                ("cmd", format!("{args:?}")),
                ("stderr", String::from_utf8_lossy(&out.stderr).to_string()),
            ],
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// app 域判定：path 在 ~/.agentloom 下（canonicalize 两边·防 macOS /var→/private/var symlink）。
#[allow(dead_code)]
pub(crate) fn is_app_domain_path(p: &Path) -> bool {
    let root = home_dir().join(".agentloom");
    let root = std::fs::canonicalize(&root).unwrap_or(root);
    let p = match std::fs::canonicalize(p) {
        Ok(c) => c,
        Err(_) => return false,
    };
    if p.starts_with(&root) {
        return true;
    }

    #[cfg(test)]
    {
        return test_app_domain_paths()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|root| p.starts_with(root));
    }

    #[cfg(not(test))]
    false
}

#[cfg(test)]
fn test_app_domain_paths() -> &'static std::sync::Mutex<Vec<PathBuf>> {
    static PATHS: std::sync::OnceLock<std::sync::Mutex<Vec<PathBuf>>> = std::sync::OnceLock::new();
    PATHS.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// Unit-test fixture hook: explicitly label a temporary repository as app-owned.
/// Production builds have no equivalent override; user-repo rejection tests must not call this.
#[cfg(test)]
pub(crate) fn mark_test_app_domain(path: &Path) {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut paths = test_app_domain_paths()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if !paths.contains(&path) {
        paths.push(path);
    }
}

/// 任何 app 侧 git 写机器的统一 fail-closed 边界。
pub(crate) fn assert_app_domain_path(path: &Path, operation: &str) -> Result<(), String> {
    if is_app_domain_path(path) {
        return Ok(());
    }
    Err(crate::ui_msg::al_err(
        "wt.write.outsideAppDomain",
        &[
            ("operation", operation.to_string()),
            ("path", path.display().to_string()),
        ],
    ))
}

/// Distinguish a consistent workspace from divergence requiring reconciliation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileVerdict {
    Clean,
    Diverged { reason: String },
}

/// Check legacy git ledger consistency without mutating the repository.
///
/// last active row 的 post_head 存在（rev-parse --verify）、是 HEAD 祖先
/// （merge-base --is-ancestor）、worktree 干净（status --porcelain 空）；任一不满足 → Diverged。
/// last_post_head=None（无 active row）时只校验 worktree 干净。
/// 跑一条 git 命令、只关心是否成功（退出码 0）。spawn 失败或退出码非 0 都返 false。
/// 仅用于 reconcile 里 fail-closed 的谓词校验（exists / is-ancestor）：调用方把 false 视为「不满足」→ Diverged。
fn git_ok(dir: &Path, args: &[&str]) -> bool {
    git_read_output(dir, args)
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn reconcile(wt: &Path, last_post_head: Option<&str>) -> ReconcileVerdict {
    // worktree 必须干净——安全 gate fail-closed：仅「git status 成功 + 输出空」算干净，
    // git 失败（.git 损坏 / 非 repo / gitdir 链断）或退出码非 0 → Diverged，绝不放行。
    let out = match git_read_output(wt, &["status", "--porcelain"]) {
        Ok(o) => o,
        Err(e) => {
            return ReconcileVerdict::Diverged {
                reason: crate::ui_msg::al_err(
                    "wt.session.gitStatusSpawnFailed",
                    &[("detail", e.to_string())],
                ),
            }
        }
    };
    if !out.status.success() {
        return ReconcileVerdict::Diverged {
            reason: crate::ui_msg::al_err(
                "wt.session.gitStatusFailed",
                &[("detail", String::from_utf8_lossy(&out.stderr).to_string())],
            ),
        };
    }
    if !String::from_utf8_lossy(&out.stdout).trim().is_empty() {
        return ReconcileVerdict::Diverged {
            reason: crate::ui_msg::al_err("wt.session.worktreeDirty", &[]),
        };
    }
    let Some(post_head) = last_post_head else {
        return ReconcileVerdict::Clean;
    };
    // post_head 必须存在
    if !git_ok(wt, &["rev-parse", "--verify", "--quiet", post_head]) {
        return ReconcileVerdict::Diverged {
            reason: crate::ui_msg::al_err(
                "wt.session.postHeadMissing",
                &[("postHead", post_head.to_string())],
            ),
        };
    }
    // post_head 必须是 HEAD 祖先
    if !git_ok(wt, &["merge-base", "--is-ancestor", post_head, "HEAD"]) {
        return ReconcileVerdict::Diverged {
            reason: crate::ui_msg::al_err(
                "wt.session.postHeadNotAncestor",
                &[("postHead", post_head.to_string())],
            ),
        };
    }
    ReconcileVerdict::Clean
}

fn worktree_registered(repo: &Path, wt: &Path) -> Result<bool, String> {
    let out = git_read_output(repo, &["worktree", "list", "--porcelain"]).map_err(|e| {
        crate::ui_msg::al_err("wt.git.worktreeListFailed", &[("detail", e.to_string())])
    })?;
    // 🔴 M1 fail-closed(codex+opus 双审):检退出码·git 非 0(损坏 repo 等)→Err·别把
    //    「无法确认是否注册」当「未注册」放行 I4/C4 守卫(否则是 fail-closed 底座上的 fail-open 缝)。
    if !out.status.success() {
        return Err(crate::ui_msg::al_err(
            "wt.git.worktreeListNonZero",
            &[
                ("exitCode", format!("{:?}", out.status.code())),
                ("stderr", String::from_utf8_lossy(&out.stderr).to_string()),
            ],
        ));
    }
    let s = String::from_utf8_lossy(&out.stdout);
    // canonicalize 两边再比：git 输出 canonical 路径(macOS /var→/private/var symlink), 直接字符串比会漏判
    let target = std::fs::canonicalize(wt).unwrap_or_else(|_| wt.to_path_buf());
    Ok(s.lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .any(|p| {
            std::fs::canonicalize(Path::new(p)).unwrap_or_else(|_| PathBuf::from(p)) == target
        }))
}

fn nul_paths(output: &str) -> Vec<String> {
    output
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect()
}

// ===== workspace dispatch（lib.rs 唯一调用入口 · 按 namespace.kind 路由）=====

/// cluster L Phase 3 plan C2-A：按 workspace 类型路由。
/// 往会话 worktree 的 git `info/exclude` 写一行 `.myagenthubs/`，让 git 在所有读 worktree 的地方
/// （status/reconcile/ls-files）忽略 harness sidecar 写进 worktree 的内部 journal。
/// 用 `git rev-parse --git-path info/exclude` 解析路径（standalone repo 与 linked worktree 都对）。
/// 幂等：已含则不重复写。失败不致命（journal 不影响 worktree 本身可用，仅退化为旧行为）。
/// 注意：info/exclude 只忽略**未跟踪**文件——历史上已被 commit 的 journal 仍追踪（旧脏会话另说）。
fn exclude_journal_in(wt: &Path) {
    let Ok(wt) = canonical_managed_worktree(wt) else {
        return;
    };
    crate::attachments::exclude::ensure_git_exclude_line(&wt, ".myagenthubs/");
}

pub fn ensure_workspace(
    session_id: &str,
    repo_path: Option<&Path>,
    is_local: bool,
) -> Result<PathBuf, String> {
    let wt = if is_local {
        ensure_worktree_for_default_in(&local_sessions_root(), session_id)?
    } else {
        let repo = repo_path.ok_or("github_org session 缺 repo path")?;
        ensure_worktree_in(&default_root(), repo, session_id)?
    };
    // 每次 ensure 都幂等写 exclude：覆盖新建 + 复用（含修复前建的旧 worktree），且在 reconcile 之前生效。
    exclude_journal_in(&wt);
    crate::attachments::exclude::ensure_agentloom_dir_excluded(&wt);
    Ok(wt)
}

pub fn review_workspace(
    session_id: &str,
    repo_path: Option<&Path>,
    is_local: bool,
) -> Result<Review, String> {
    if is_local {
        review_default_in(&local_sessions_root(), session_id)
    } else {
        let repo = repo_path.ok_or("github_org session 缺 repo path")?;
        review_in(&default_root(), repo, session_id)
    }
}

#[cfg(test)]
fn ensure_worktree_dispatch_in(
    sessions_root: &Path,
    wt_root: &Path,
    session_id: &str,
    repo_path: Option<&Path>,
) -> Result<PathBuf, String> {
    match repo_path {
        Some(repo) => ensure_worktree_in(wt_root, repo, session_id),
        None => ensure_worktree_for_default_in(sessions_root, session_id),
    }
}

/// review dispatch：默认 session 用 sessions_root；关联项目 session 用 wt_root + repo。
#[cfg(test)]
fn review_dispatch_in(
    sessions_root: &Path,
    wt_root: &Path,
    session_id: &str,
    repo_path: Option<&Path>,
) -> Result<Review, String> {
    match repo_path {
        Some(repo) => review_in(wt_root, repo, session_id),
        None => review_default_in(sessions_root, session_id),
    }
}

/// 默认 session 的 review：worktree 本身就是 git repo（git init 时建）。
fn review_default_in(sessions_root: &Path, session_id: &str) -> Result<Review, String> {
    let safe = safe_id(session_id);
    if safe.is_empty() {
        return Ok(Review::empty());
    }
    let wt = sessions_root.join(&safe);
    if !wt.exists() {
        return Ok(Review::empty());
    }
    let base_ref = format!("refs/agentloom/base/{safe}");
    let base_ok = git_read_output(&wt, &["rev-parse", "--verify", "--quiet", &base_ref])
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !base_ok {
        return Ok(Review::empty());
    }
    review_working_tree_at(&wt, &base_ref)
}

#[cfg(test)]
pub(crate) fn test_home_lock() -> std::sync::MutexGuard<'static, ()> {
    static HOME_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    HOME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests;
