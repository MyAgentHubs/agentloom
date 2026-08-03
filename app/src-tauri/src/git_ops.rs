//! b2b「把活发出去」后端基础（plan 2026-06-21-tc3-b2b-changebar-push-pr · Slice A Task A1）。
//!
//! 提供：
//! - gh 多账户身份解析（session → gh 账户登录名 / token）
//! - repo 级 commit 身份切换
//! - git/gh 命令行参数组装纯函数（可单测 · 不执行）
//! - 自写的写操作 git runner + gh runner；只读 git 查询走 worktree 的统一加固封装
//!
//! push / PR / publish 的 run 函数在后续 task（A2）接线。

/// session → gh 账户登录名。
///
/// namespace_id 形如 `gh:<owner>` → `Some(owner)`；`local`（或不以 `gh:` 开头）→ `None`。
/// 会话不存在 / 无 namespace 也按 `None` 处理（本地会话语义）。
pub fn resolve_gh_account_for_session(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<Option<String>, String> {
    let ns = crate::db::get_session_namespace_id(conn, session_id)
        .map_err(|e| format!("DB_ERROR:{e}"))?;
    Ok(ns.and_then(|id| id.strip_prefix("gh:").map(str::to_string)))
}

/// session → gh token。无 gh 账户（本地会话）→ `Err("LOCAL_SESSION_NO_TOKEN")`。
///
/// namespace 建立时把 git remote 属主直接存成登录名（见 `lib.rs` ensure_github_namespace 调用点），
/// 但属主可能是 GitHub **组织**（如 `MyAgentHubs`）而非真实登录账户——`gh auth token --user <组织名>`
/// 必然失败。这里做两级回退，真正根治要等 namespace 建立时补「账户登录字段」（白天设计单，本刀不做 schema）。
pub fn gh_token_for_session(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<String, String> {
    let login = resolve_gh_account_for_session(conn, session_id)?
        .ok_or_else(|| "LOCAL_SESSION_NO_TOKEN".to_string())?;
    gh_token_with_fallback(
        &login,
        crate::github::gh_token_for,
        crate::github::read_gh_accounts,
    )
}

/// 两级回退核（纯逻辑·靠注入 `token_fetcher`/`accounts_fetcher` 可测，不真跑 `gh`）：
/// ① 先按 `owner` 本身取 token（属主本来就是登录账户 → 零变化，直接成功）；
/// ② 失败且是 `NO_TOKEN:` 类 → 读本机已登录账户，找 active 账户重试一次；
/// 两级都失败 → 错误升级为人话（含属主名 + 已登录账户列表），**保留 `NO_TOKEN:` 前缀**——
/// 前端 `message.startsWith("NO_TOKEN")` / `message.split(":")[1]` 两处消费方兼容（第二段仍是 owner）。
fn gh_token_with_fallback(
    owner: &str,
    token_fetcher: impl Fn(&str) -> Result<String, String>,
    accounts_fetcher: impl Fn() -> Result<Vec<crate::github::GhAccount>, String>,
) -> Result<String, String> {
    let first_err = match token_fetcher(owner) {
        Ok(tok) => return Ok(tok),
        Err(e) => e,
    };
    if !first_err.starts_with("NO_TOKEN:") {
        // GH_MISSING / TIMEOUT / GH_COMMAND_FAILED 类不是「属主非登录账户」——回退无意义，原样透传。
        return Err(first_err);
    }
    let accounts_result = accounts_fetcher();
    let accounts: &[crate::github::GhAccount] = accounts_result.as_deref().unwrap_or(&[]);
    if let Some(active) = accounts.iter().find(|a| a.active) {
        // active 就是 owner 本身时，第一级已经拿 owner 试过且失败——用同一登录名重试第二次
        // 只是多等一个 gh 子进程超时窗口、结果必然相同，直接跳过。
        if active.login != owner {
            if let Ok(tok) = token_fetcher(&active.login) {
                eprintln!(
                    "[gh_token] namespace 属主 {owner} 非登录账户·已回退到活跃账户 {}",
                    active.login
                );
                return Ok(tok);
            }
        }
    }
    // 区分「账户列表读取失败」vs「真的没有已登录账户」——前者不能谎报成 (无) 误导用户去重登。
    let list_desc = match &accounts_result {
        Err(e) => format!("（账户列表读取失败：{e}）"),
        Ok(list) if list.is_empty() => "(无)".to_string(),
        Ok(list) => list
            .iter()
            .map(|a| a.login.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    };
    Err(format!(
        "NO_TOKEN:{owner}:属主 {owner} 可能是组织而非登录账户；本机 gh 已登录账户：{list_desc}"
    ))
}

/// push / PR / publish 等不可逆外部副作用必须携带 UI 层的本次显式确认。
pub fn require_explicit_confirmation(confirmed: bool, operation: &str) -> Result<(), String> {
    if confirmed {
        Ok(())
    } else {
        Err(crate::ui_msg::al_err(
            "delivery.confirmationRequired",
            &[("operation", operation.to_string())],
        ))
    }
}

/// 纯函数：组装 `gh pr create` 命令行参数（不含 `gh` 本身）。
///
/// 含 `--head`/`--base`/`--title`；`body` 为 `Some` 则追加 `--body`。
pub fn gh_pr_create_plan(head: &str, base: &str, title: &str, body: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "pr".to_string(),
        "create".to_string(),
        "--head".to_string(),
        head.to_string(),
        "--base".to_string(),
        base.to_string(),
        "--title".to_string(),
        title.to_string(),
    ];
    if let Some(b) = body {
        args.push("--body".to_string());
        args.push(b.to_string());
    }
    args
}

/// 纯函数：组装 `gh repo create` 命令行参数（不含 `gh` 本身）。
///
/// 含 `name`；`private == true` 追加 `--private`，否则追加 `--public`。
pub fn gh_repo_create_plan(name: &str, private: bool) -> Vec<String> {
    vec![
        "repo".to_string(),
        "create".to_string(),
        name.to_string(),
        if private { "--private" } else { "--public" }.to_string(),
    ]
}

/// 自写 git runner：执行 `git -C <repo> <args...>`，返 trimmed stdout；失败带 stderr。
///
/// worktree::run_git 是私有不可 import，故自带一份。
fn run_git_in(repo: &std::path::Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = crate::proc::command("git");
    cmd.arg("-C").arg(repo).args(args);
    let out = cmd.output().map_err(|_| "GIT_MISSING".to_string())?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("GIT_FAILED:{}", stderr.trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// 自写 gh runner：执行 `gh <args...>`（cwd=repo）+ 注入 GH_TOKEN env；返 trimmed stdout；失败带 stderr（token 脱敏）。
///
/// env 注入照 github::clone_repo_https（GH_TOKEN + GH_PROMPT_DISABLED + GIT_TERMINAL_PROMPT）。
fn run_gh_in(repo: &std::path::Path, args: &[&str], gh_token: &str) -> Result<String, String> {
    let out = crate::proc::command("gh")
        .current_dir(repo)
        .args(args)
        .env("GH_TOKEN", gh_token)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|_| "GH_MISSING".to_string())?;
    if !out.status.success() {
        let raw = String::from_utf8_lossy(&out.stderr);
        let redacted = raw.replace(gh_token, "***");
        return Err(format!("GH_FAILED:{}", redacted.trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// 真执行 `git push -u <remote> <branch>`。返 trimmed stdout。
pub fn git_push(
    repo: &std::path::Path,
    remote: &str,
    branch: &str,
    confirmed: bool,
) -> Result<String, String> {
    require_explicit_confirmation(confirmed, "git push")?;
    let plan = [
        "push".to_string(),
        "-u".to_string(),
        remote.to_string(),
        branch.to_string(),
    ];
    let args: Vec<&str> = plan.iter().map(String::as_str).collect();
    run_git_in(repo, &args)
}

/// 真执行 `gh pr create ...`（复用 gh_pr_create_plan + run_gh_in）。
///
/// `gh pr create` 成功后会把新建 PR 的 url 打到 stdout（通常整段 stdout 就是那行 url）。
/// 故直接返回 trimmed stdout——若多行则取末行里含 `pull/` 的那行（更稳·容忍 gh 在前面打提示）。
pub fn gh_pr_create(
    repo: &std::path::Path,
    head: &str,
    base: &str,
    title: &str,
    body: Option<&str>,
    gh_token: &str,
) -> Result<String, String> {
    let plan = gh_pr_create_plan(head, base, title, body);
    let args: Vec<&str> = plan.iter().map(String::as_str).collect();
    let stdout = run_gh_in(repo, &args, gh_token)?;
    Ok(pick_github_url(&stdout, "pull/").unwrap_or(stdout))
}

/// 真执行 `gh repo create <name> [--private|--public] --source . --remote origin --push`
/// （一步建仓 + 设 origin + 推），复用 gh_repo_create_plan 的 name/private flag。返回 repo url。
///
/// gh repo create 成功后 stdout 通常含 `https://github.com/<owner>/<name>`——取含 github.com 的那行。
pub fn gh_repo_create(
    repo: &std::path::Path,
    name: &str,
    private: bool,
    gh_token: &str,
) -> Result<String, String> {
    let mut plan = gh_repo_create_plan(name, private);
    // publish = 建仓 + 把本地 source 绑 origin + push（一步到位）。
    plan.push("--source".to_string());
    plan.push(".".to_string());
    plan.push("--remote".to_string());
    plan.push("origin".to_string());
    plan.push("--push".to_string());
    let args: Vec<&str> = plan.iter().map(String::as_str).collect();
    let stdout = run_gh_in(repo, &args, gh_token)?;
    Ok(pick_github_url(&stdout, "github.com").unwrap_or(stdout))
}

/// 从多行文本里挑含 `needle` 的那行里的 `https://...` url（取第一段空白分隔 token 中以 http 开头者）。
/// 找不到返 None（让上层 fallback 到整段 stdout）。
fn pick_github_url(text: &str, needle: &str) -> Option<String> {
    text.lines().filter(|l| l.contains(needle)).find_map(|l| {
        l.split_whitespace()
            .find(|tok| tok.starts_with("http"))
            .map(str::to_string)
    })
}

/// 纯函数：把 `git remote get-url origin` 的结果判成「有没有 origin 远程」。
///
/// `Ok` 且 trim 后非空 → `true`（有远程）；`Err`（无 origin / git 失败）或空 → `false`。
pub fn parse_has_remote(remote_url_result: Result<String, String>) -> bool {
    matches!(remote_url_result, Ok(url) if !url.trim().is_empty())
}

/// 跑 `git remote get-url origin` 喂 `parse_has_remote`，判断 repo 有没有 origin 远程。
pub fn has_remote(repo: &std::path::Path) -> bool {
    parse_has_remote(
        crate::worktree::git_checked_stdout(repo, &["remote", "get-url", "origin"])
            .map(|stdout| stdout.trim().to_string()),
    )
}

/// 纯函数：组装改动条 repo 标签「<前缀> · <repo 短名>」。
///
/// `prefix` = gh 账户名 或 namespace 名（如 `agentloom`）；
/// `repo_name_or_path` = repo 记录的 name 或 path basename（可能含 `owner/`）——只取末段短名。
/// 形态参原型「agentloom · ai-cat-pet」。
pub fn compose_repo_label(prefix: &str, repo_name_or_path: &str) -> String {
    let short = repo_short_name(repo_name_or_path);
    format!("{prefix} · {short}")
}

/// 纯函数：从 `owner/repo`、`repo`、或路径里取 repo 短名（末段、去 `.git` 后缀）。
pub fn repo_short_name(name_or_path: &str) -> String {
    name_or_path
        .trim_end_matches('/')
        .rsplit(['/', '\\'])
        .find(|s| !s.is_empty())
        .unwrap_or(name_or_path)
        .trim_end_matches(".git")
        .to_string()
}

/// 幂等护栏（评审阻断·务必照做）：本 run 是否还需要落地。
///
/// 落地成功后 staging 分支会被删（cleanup_run_workspaces → delete_staging_branch），
/// 而 apply_staging_ff_only 在没有 staging 时会报错——所以严禁无条件先 apply。
/// per-run 判断：`latest_landing_commit(conn, session, run).is_none()` → 还没落地 → 需要落地。
pub fn needs_landing(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
) -> Result<bool, String> {
    Ok(crate::db::latest_landing_commit(conn, session_id, run_id)
        .map_err(|e| format!("DB_ERROR:{e}"))?
        .is_none())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_gh_account_returns_owner_for_gh_namespace() {
        let conn = crate::test_support::mem_db();
        crate::namespaces_repo::ensure_github_namespace(&conn, "gh:alice", "alice")
            .expect("seed gh:alice namespace");
        // 为 gh:alice 建 repo（create_session 的 repo_id/namespace_id 是 &str · FK 需有效 repo）
        conn.execute(
            "INSERT INTO repos (id, namespace_id, source, name, path, status, added_at) \
             VALUES ('alice-repo', 'gh:alice', 'github', 'alice/foo', '/tmp/x', 'active', 0)",
            [],
        )
        .expect("seed gh repo");
        crate::db::create_session(&conn, "s-gh", "T", "alice-repo", "gh:alice")
            .expect("create gh session");

        let got = resolve_gh_account_for_session(&conn, "s-gh").expect("resolve");
        assert_eq!(got, Some("alice".to_string()));
    }

    #[test]
    fn resolve_gh_account_returns_none_for_local_session() {
        let conn = crate::test_support::mem_db();
        crate::db::create_session(&conn, "s-local", "T", "local-default", "local")
            .expect("create local session");

        let got = resolve_gh_account_for_session(&conn, "s-local").expect("resolve");
        assert_eq!(got, None);
    }

    #[test]
    fn gh_token_with_fallback_first_level_succeeds_no_fallback() {
        // 属主本来就是登录账户 → 第一级直接成功、不触发回退（行为零变化钉子）。
        use std::cell::Cell;
        let accounts_called = Cell::new(false);
        let got = gh_token_with_fallback(
            "alice",
            |login| {
                assert_eq!(login, "alice");
                Ok("tok-alice".to_string())
            },
            || {
                accounts_called.set(true);
                Ok(vec![])
            },
        )
        .expect("first level token");
        assert_eq!(got, "tok-alice");
        assert!(
            !accounts_called.get(),
            "属主即登录账户时不应触发回退读账户列表"
        );
    }

    #[test]
    fn gh_token_with_fallback_falls_back_to_active_account() {
        // 属主非登录账户（如组织名）+ 本机有 active 账户 → 回退成功，返回 active 的 token。
        let got = gh_token_with_fallback(
            "MyAgentHubs",
            |login| match login {
                "MyAgentHubs" => Err("NO_TOKEN:MyAgentHubs".to_string()),
                "impanda-cookie" => Ok("tok-active".to_string()),
                other => panic!("unexpected login {other}"),
            },
            || {
                Ok(vec![
                    crate::github::GhAccount {
                        login: "octocat".to_string(),
                        active: false,
                    },
                    crate::github::GhAccount {
                        login: "impanda-cookie".to_string(),
                        active: true,
                    },
                ])
            },
        )
        .expect("fallback token");
        assert_eq!(got, "tok-active");
    }

    #[test]
    fn gh_token_with_fallback_both_levels_fail_reports_human_message() {
        // 两级都失败 → 错误升级为人话：含属主名 + 本机已登录账户列表；仍保留 NO_TOKEN: 前缀兼容前端。
        let err = gh_token_with_fallback(
            "MyAgentHubs",
            |_login| Err("NO_TOKEN:whatever".to_string()),
            || {
                Ok(vec![crate::github::GhAccount {
                    login: "impanda-cookie".to_string(),
                    active: true,
                }])
            },
        )
        .unwrap_err();
        assert!(
            err.starts_with("NO_TOKEN:MyAgentHubs"),
            "保留前缀兼容前端解析: {err}"
        );
        assert!(err.contains("MyAgentHubs"), "含属主名: {err}");
        assert!(err.contains("impanda-cookie"), "含已登录账户列表: {err}");
    }

    #[test]
    fn gh_token_with_fallback_passes_through_non_no_token_error_without_fallback() {
        // 非 NO_TOKEN: 类错误（GH_MISSING/TIMEOUT/GH_COMMAND_FAILED）不是「属主非登录账户」信号——
        // 原样透传，且不该白跑一次读账户列表。
        use std::cell::Cell;
        let accounts_called = Cell::new(false);
        let err = gh_token_with_fallback(
            "alice",
            |_login| Err("GH_MISSING".to_string()),
            || {
                accounts_called.set(true);
                Ok(vec![])
            },
        )
        .unwrap_err();
        assert_eq!(err, "GH_MISSING");
        assert!(
            !accounts_called.get(),
            "非 NO_TOKEN 错误不应触发回退读账户列表"
        );
    }

    #[test]
    fn gh_token_with_fallback_reports_accounts_read_failure_not_as_empty() {
        // accounts_fetcher 本身失败（如 gh auth status 超时）≠「真的没有已登录账户」——
        // 不能谎报成 (无) 误导用户去重登，要如实带上读取失败详情。
        let err = gh_token_with_fallback(
            "MyAgentHubs",
            |_login| Err("NO_TOKEN:whatever".to_string()),
            || Err("TIMEOUT".to_string()),
        )
        .unwrap_err();
        assert!(
            err.contains("账户列表读取失败"),
            "应区分「读取失败」与「真空列表」: {err}"
        );
        assert!(err.contains("TIMEOUT"), "应带上原始错误详情: {err}");
        assert!(!err.contains("(无)"), "不应把读取失败谎报成空列表: {err}");
    }

    #[test]
    fn gh_token_with_fallback_skips_retry_when_active_account_is_owner_itself() {
        // active 账户登录名恰好就是 owner 本身：第一级已经用 owner 试过且失败了，
        // 同名重试第二次结果必然相同，只会多等一个 gh 子进程超时窗口——应跳过。
        use std::cell::Cell;
        let token_calls = Cell::new(0);
        let err = gh_token_with_fallback(
            "alice",
            |login| {
                token_calls.set(token_calls.get() + 1);
                assert_eq!(login, "alice", "active==owner 时不应用同名重试第二次");
                Err("NO_TOKEN:alice".to_string())
            },
            || {
                Ok(vec![crate::github::GhAccount {
                    login: "alice".to_string(),
                    active: true,
                }])
            },
        )
        .unwrap_err();
        assert_eq!(
            token_calls.get(),
            1,
            "active 即 owner 时应跳过第二级、token_fetcher 只应被调一次"
        );
        assert!(err.starts_with("NO_TOKEN:alice"));
    }

    #[test]
    fn git_push_rejects_missing_confirmation_before_touching_repo() {
        let err = git_push(
            std::path::Path::new("/definitely/missing/user-repo"),
            "origin",
            "feat",
            false,
        )
        .unwrap_err();
        assert_eq!(
            err,
            r#"AL_ERR:delivery.confirmationRequired:{"operation":"git push"}"#
        );
    }

    #[test]
    fn gh_pr_create_plan_includes_required_flags() {
        let args = gh_pr_create_plan("feat", "main", "T", Some("B"));
        for needle in [
            "--head", "feat", "--base", "main", "--title", "T", "--body", "B",
        ] {
            assert!(
                args.iter().any(|a| a == needle),
                "expected {needle:?} in {args:?}"
            );
        }
    }

    #[test]
    fn gh_pr_create_plan_omits_body_when_none() {
        let args = gh_pr_create_plan("feat", "main", "T", None);
        assert!(!args.iter().any(|a| a == "--body"), "no --body when None");
    }

    #[test]
    fn needs_landing_true_before_landing_false_after() {
        let conn = crate::test_support::mem_db();
        crate::namespaces_repo::ensure_github_namespace(&conn, "gh:bob", "bob")
            .expect("seed gh:bob namespace");
        conn.execute(
            "INSERT INTO repos (id, namespace_id, source, name, path, status, added_at) \
             VALUES ('bob-repo', 'gh:bob', 'github', 'bob/foo', '/tmp/x', 'active', 0)",
            [],
        )
        .expect("seed gh repo");
        crate::db::create_session(&conn, "s-land", "T", "bob-repo", "gh:bob")
            .expect("create gh session");

        // 未落地 → 需要落地。
        assert!(
            needs_landing(&conn, "s-land", "r1").expect("needs_landing pre"),
            "未落地时 needs_landing 应为 true"
        );

        // 记一笔本 run 的 LandingCommit（= 已落地）。
        crate::db::insert_landing_commit(
            &conn,
            &crate::db::LandingCommit {
                id: "lc-1".into(),
                session_id: "s-land".into(),
                run_id: "r1".into(),
                artifact_id: None,
                pre_head: "aaaa".into(),
                landed_head: "bbbb".into(),
                commit_count: 1,
                files_changed: 1,
                insertions: 1,
                deletions: 0,
                created_at: 1,
            },
        )
        .expect("insert landing commit");

        // 已落地 → 不再需要落地（per-run 幂等护栏）。
        assert!(
            !needs_landing(&conn, "s-land", "r1").expect("needs_landing post"),
            "已落地时 needs_landing 应为 false"
        );
        // per-run：另一 run 仍未落地 → true（不是 session 级）。
        assert!(
            needs_landing(&conn, "s-land", "r2").expect("needs_landing other run"),
            "同会话另一 run 未落地 → needs_landing 应为 true（per-run 非 session 级）"
        );
    }

    #[test]
    fn pick_github_url_extracts_url_from_matching_line() {
        let text = "https://github.com/o/r/pull/7";
        assert_eq!(
            pick_github_url(text, "pull/"),
            Some("https://github.com/o/r/pull/7".to_string())
        );
        let multi = "Warning: something\nCreated repo\nhttps://github.com/o/r";
        assert_eq!(
            pick_github_url(multi, "github.com"),
            Some("https://github.com/o/r".to_string())
        );
        assert_eq!(pick_github_url("no url here", "pull/"), None);
    }

    #[test]
    fn parse_has_remote_truth_table() {
        assert!(parse_has_remote(Ok("git@github.com:o/r.git".to_string())));
        assert!(!parse_has_remote(Ok("  ".to_string())));
        assert!(!parse_has_remote(Err("no origin".to_string())));
    }

    #[test]
    fn has_remote_false_for_fresh_repo_true_after_add() {
        let td = tempfile::tempdir().expect("tempdir");
        let repo = td.path();
        run_git_in(repo, &["init"]).expect("git init");
        assert!(!has_remote(repo), "fresh repo 无 origin → false");

        run_git_in(repo, &["remote", "add", "origin", "git@github.com:o/r.git"])
            .expect("add origin");
        assert!(has_remote(repo), "加了 origin → true");
    }

    #[test]
    fn repo_short_name_takes_last_segment() {
        assert_eq!(repo_short_name("alice/ai-cat-pet"), "ai-cat-pet");
        assert_eq!(repo_short_name("ai-cat-pet"), "ai-cat-pet");
        assert_eq!(repo_short_name("/Users/a/code/ai-cat-pet"), "ai-cat-pet");
        assert_eq!(repo_short_name("o/r.git"), "r");
    }

    #[test]
    fn compose_repo_label_shape() {
        assert_eq!(
            compose_repo_label("agentloom", "agentloom/ai-cat-pet"),
            "agentloom · ai-cat-pet"
        );
    }

    #[test]
    fn gh_repo_create_plan_private_flag() {
        let priv_args = gh_repo_create_plan("foo", true);
        assert!(priv_args.iter().any(|a| a == "foo"), "contains name");
        assert!(
            priv_args.iter().any(|a| a == "--private"),
            "private contains --private"
        );

        let pub_args = gh_repo_create_plan("foo", false);
        assert!(
            !pub_args.iter().any(|a| a == "--private"),
            "public omits --private"
        );
    }
}
