// Lead delivery MCP tool definitions moved from lib.rs.

use super::*;

pub(super) fn parse_goal_title_arg(args: &serde_json::Value) -> Option<String> {
    args.get("goal_title")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

fn sanitize_commit_preview_path(path: &std::path::Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|character| {
            if character.is_control() {
                '?'
            } else {
                character
            }
        })
        .collect()
}

pub(super) fn format_lead_commit_preview(
    selection: &commit_broker::CommittableSelection,
    locale: Locale,
) -> String {
    let mut question = String::from(match locale {
        Locale::Zh => "将提交：",
        Locale::En => "Will commit:",
    });
    if selection.exact_paths.is_empty() {
        question.push_str(match locale {
            Locale::Zh => "\n（无）",
            Locale::En => "\n(none)",
        });
    } else {
        for path in &selection.exact_paths {
            let sanitized = sanitize_commit_preview_path(path);
            // This preview is the one human-in-the-loop checkpoint before a repository's
            // commits are auto-approved, and a deletion is destructive/irreversible in a way a
            // plain path string doesn't convey — call it out explicitly rather than letting it
            // look identical to an add/modify.
            if selection.deleted_paths.contains(path) {
                let deleted_label = match locale {
                    Locale::Zh => "删除",
                    Locale::En => "deleted",
                };
                question.push_str(&format!("\n- [{deleted_label}] {sanitized}"));
            } else {
                question.push_str(&format!("\n- {sanitized}"));
            }
        }
    }

    question
}

fn lead_commit_confirmation_copy(locale: Locale) -> (&'static str, &'static str, &'static str) {
    match locale {
        Locale::Zh => ("提交", "取消", "提交前请核对本次请求将提交的文件清单。"),
        Locale::En => (
            "Commit",
            "Cancel",
            "Check the file list this request is about to commit.",
        ),
    }
}

pub(super) fn lead_commit_confirmation_args(
    question: String,
    locale: Locale,
) -> lead_tools::AskUserArgs {
    let (confirm_label, cancel_label, rationale) = lead_commit_confirmation_copy(locale);
    lead_tools::AskUserArgs {
        question,
        options: vec![confirm_label.into(), cancel_label.into()],
        recommended: Some(confirm_label.into()),
        rationale: Some(rationale.into()),
    }
}

pub(super) fn confirmation_option_labels(
    args: &lead_tools::AskUserArgs,
    context: &str,
) -> Result<(String, String), String> {
    match args.options.as_slice() {
        [confirm_label, cancel_label] => Ok((confirm_label.clone(), cancel_label.clone())),
        options => Err(format!(
            "{context}: confirmation requires exactly two options, got {}",
            options.len()
        )),
    }
}

pub(super) fn lead_commit_confirmation_is_cancelled(
    answer: &str,
    confirm_label: &str,
    cancel_label: &str,
) -> Result<bool, String> {
    if answer == cancel_label {
        return Ok(true);
    }
    if answer != confirm_label {
        return Err(format!("commit: unexpected confirmation answer: {answer}"));
    }
    Ok(false)
}

pub(super) fn lead_commit_requires_preview(authorized: bool) -> bool {
    !authorized
}

pub(super) fn finish_commit_ledger(
    conn: &rusqlite::Connection,
    worktree: &std::path::Path,
    session_id: &str,
    run_id: &str,
    pre_head: &str,
    sha: &str,
) -> Result<Option<String>, String> {
    let stats = worktree::landing_stats(worktree, pre_head, sha);
    let (files_changed, insertions, deletions) = stats
        .as_ref()
        .map(|value| {
            (
                Some(value.files_changed.max(0) as u64),
                Some(value.insertions.max(0) as u64),
                Some(value.deletions.max(0) as u64),
            )
        })
        .unwrap_or((None, None, None));
    let warning = stats.err();
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    db::record_run_commit(
        conn,
        session_id,
        run_id,
        sha,
        files_changed,
        insertions,
        deletions,
    )
    .map_err(|e| format!("commit {sha} succeeded, but its Review ledger update failed: {e}"))?;
    db::delete_run_commit_intent(conn, session_id, run_id).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(warning)
}

pub(super) enum DeliveryAnswer {
    Confirmed,
    Cancelled,
    Pending(serde_json::Value),
}

const DELIVERY_PENDING_NOTE: &str = "用户尚未在界面确认卡上作答。本次调用没有执行任何推送、PR 或发布动作。用户答复稍后会以用户消息出现在你的上下文里；看到确认后，你需要重新调用本工具完成交付。不要凭本次返回宣称交付已完成。";

pub(super) fn solo_delivery_confirmation_args(
    question: String,
    rationale: &str,
    locale: Locale,
) -> lead_tools::AskUserArgs {
    let (confirm_label, cancel_label) = match locale {
        Locale::Zh => ("确认", "取消"),
        Locale::En => ("Confirm", "Cancel"),
    };
    lead_tools::AskUserArgs {
        question,
        options: vec![confirm_label.into(), cancel_label.into()],
        recommended: Some(confirm_label.into()),
        rationale: Some(rationale.into()),
    }
}

fn push_delivery_confirmation_copy(
    repo_name: &str,
    branch: &str,
    locale: Locale,
) -> (String, &'static str) {
    match locale {
        Locale::Zh => (
            format!("确认推送 {repo_name} 的 {branch} 分支到 origin?"),
            "推送会更新远端仓库，执行后无法由 AgentLoom 自动撤销。",
        ),
        Locale::En => (
            format!("Push the {branch} branch of {repo_name} to origin?"),
            "Pushing will update the remote repository and cannot be automatically undone by AgentLoom afterward.",
        ),
    }
}

fn create_pr_delivery_confirmation_copy(
    repo_name: &str,
    branch: &str,
    locale: Locale,
) -> (String, &'static str) {
    match locale {
        Locale::Zh => (
            format!("确认推送 {repo_name} 的 {branch} 分支到 origin 并创建 PR?"),
            "创建 PR 会先更新远端分支，并在 GitHub 上创建公开可见的协作记录。",
        ),
        Locale::En => (
            format!("Push the {branch} branch of {repo_name} to origin and create a PR?"),
            "Creating a PR will first update the remote branch and create a publicly visible collaboration record on GitHub.",
        ),
    }
}

fn publish_delivery_confirmation_copy(
    repo_name: Option<&str>,
    private: bool,
    locale: Locale,
) -> (String, &'static str) {
    match locale {
        Locale::Zh => {
            let visibility = if private { "私有" } else { "公开" };
            let target = repo_name.unwrap_or("自动命名的仓库");
            (
                format!("确认发布为 GitHub {visibility}仓库 {target}?"),
                "发布会在 GitHub 上创建新仓库并推送本地提交，执行后无法由 AgentLoom 自动撤销。",
            )
        }
        Locale::En => {
            let visibility = if private { "private" } else { "public" };
            let target = repo_name.unwrap_or("an automatically named repository");
            (
                format!("Publish {target} as a {visibility} GitHub repository?"),
                "Publishing will create a new repository on GitHub and push local commits, and cannot be automatically undone by AgentLoom afterward.",
            )
        }
    }
}

pub(super) fn parse_solo_delivery_confirmation(
    envelope: serde_json::Value,
    confirm_label: &str,
    cancel_label: &str,
) -> Result<DeliveryAnswer, String> {
    match envelope.get("answer").and_then(|value| value.as_str()) {
        Some(answer) if answer == confirm_label => Ok(DeliveryAnswer::Confirmed),
        Some(answer) if answer == cancel_label => Ok(DeliveryAnswer::Cancelled),
        Some(other) => Err(format!("delivery: unexpected confirmation answer: {other}")),
        None if envelope.get("status").and_then(|value| value.as_str()) == Some("pending_user") => {
            Ok(DeliveryAnswer::Pending(serde_json::json!({
                "status": "pending_user",
                "note": DELIVERY_PENDING_NOTE,
            })))
        }
        None => Err("delivery: confirmation returned no answer or pending status".to_string()),
    }
}

fn ask_solo_delivery_confirmation(
    app: &AppHandle,
    session_id: &str,
    question: String,
    rationale: &str,
) -> Result<DeliveryAnswer, String> {
    let confirmation_args =
        solo_delivery_confirmation_args(question, rationale, crate::current_locale(app));
    let (confirm_label, cancel_label) = confirmation_option_labels(&confirmation_args, "delivery")?;
    let envelope = lead_tools::ask_user_bounded(
        app,
        session_id,
        confirmation_args,
        // The delivery confirmation shared by solo/lead (push/create_pr/publish) has no
        // natural identity source to pass yet.
        None,
        None,
    )?;
    parse_solo_delivery_confirmation(envelope, &confirm_label, &cancel_label)
}

pub(super) fn execute_solo_delivery_answer<F>(
    answer: DeliveryAnswer,
    execute: F,
) -> Result<serde_json::Value, String>
where
    F: FnOnce(bool) -> Result<String, String>,
{
    match answer {
        DeliveryAnswer::Cancelled => Ok(serde_json::json!({"refused": "用户取消"})),
        DeliveryAnswer::Confirmed => {
            execute(true).map(|result| serde_json::json!({"result": result}))
        }
        DeliveryAnswer::Pending(envelope) => Ok(envelope),
    }
}

fn optional_delivery_string(
    args: &serde_json::Value,
    key: &str,
    tool_name: &str,
) -> Result<Option<String>, String> {
    match args.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(|value| Some(value.to_string()))
            .ok_or_else(|| format!("{tool_name}: {key} must be a string")),
    }
}

fn repo_delivery_confirmation_target(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<(String, String), String> {
    let repo = match resolve_session_workspace(conn, session_id)? {
        SessionWorkspace::Repo(path) => path,
        SessionWorkspace::Local => return Err("LOCAL_SESSION_NOT_PUSHABLE".to_string()),
    };
    let branch = delivery_branch(&repo)?;
    let repo_name = repo
        .file_name()
        .filter(|name| !name.is_empty())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| repo.display().to_string());
    Ok((repo_name, branch))
}

pub(super) fn build_push_tool(
    app: &AppHandle,
    session_id: &str,
    run_id: &str,
) -> mcp_server::ToolDef {
    let app_push = app.clone();
    let sess_push = session_id.to_string();
    let run_push = run_id.to_string();

    mcp_server::ToolDef {
        name: "push".to_string(),
        description: LEAD_PUSH_DESCRIPTION.to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        handler: Box::new(move |_args: serde_json::Value| {
            let (repo_name, branch) = {
                let db_state = app_push.state::<Db>();
                let conn = db_state.0.lock().map_err(|e| e.to_string())?;
                repo_delivery_confirmation_target(&conn, &sess_push)?
            };
            let (question, rationale) = push_delivery_confirmation_copy(
                &repo_name,
                &branch,
                crate::current_locale(&app_push),
            );
            let answer =
                ask_solo_delivery_confirmation(&app_push, &sess_push, question, rationale)?;
            execute_solo_delivery_answer(answer, |confirmed| {
                let db_state = app_push.state::<Db>();
                let conn = db_state.0.lock().map_err(|e| e.to_string())?;
                push_run_inner(&conn, &sess_push, &run_push, confirmed)
            })
        }),
    }
}

pub(super) fn build_create_pr_tool(
    app: &AppHandle,
    session_id: &str,
    run_id: &str,
) -> mcp_server::ToolDef {
    let app_pr = app.clone();
    let sess_pr = session_id.to_string();
    let run_pr = run_id.to_string();

    mcp_server::ToolDef {
        name: "create_pr".to_string(),
        description: LEAD_CREATE_PR_DESCRIPTION.to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "title": {"type": "string"},
                "body": {"type": "string"}
            },
            "additionalProperties": false
        }),
        handler: Box::new(move |args: serde_json::Value| {
            let title = optional_delivery_string(&args, "title", "create_pr")?;
            let body = optional_delivery_string(&args, "body", "create_pr")?;
            let (repo_name, branch) = {
                let db_state = app_pr.state::<Db>();
                let conn = db_state.0.lock().map_err(|e| e.to_string())?;
                repo_delivery_confirmation_target(&conn, &sess_pr)?
            };
            let (question, rationale) = create_pr_delivery_confirmation_copy(
                &repo_name,
                &branch,
                crate::current_locale(&app_pr),
            );
            let answer = ask_solo_delivery_confirmation(&app_pr, &sess_pr, question, rationale)?;
            execute_solo_delivery_answer(answer, |confirmed| {
                let db_state = app_pr.state::<Db>();
                let conn = db_state.0.lock().map_err(|e| e.to_string())?;
                create_pr_run_inner(&conn, &sess_pr, &run_pr, title, body, confirmed)
            })
        }),
    }
}

pub(super) fn build_publish_tool(
    app: &AppHandle,
    session_id: &str,
    run_id: &str,
) -> mcp_server::ToolDef {
    let app_publish = app.clone();
    let sess_publish = session_id.to_string();
    let run_publish = run_id.to_string();

    mcp_server::ToolDef {
        name: "publish".to_string(),
        description: LEAD_PUBLISH_DESCRIPTION.to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "repo_name": {"type": "string"},
                "private": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
        handler: Box::new(move |args: serde_json::Value| {
            let repo_name = optional_delivery_string(&args, "repo_name", "publish")?;
            let private = match args.get("private") {
                None | Some(serde_json::Value::Null) => None,
                Some(value) => Some(
                    value
                        .as_bool()
                        .ok_or_else(|| "publish: private must be a boolean".to_string())?,
                ),
            };
            let (question, rationale) = publish_delivery_confirmation_copy(
                repo_name.as_deref(),
                private.unwrap_or(true),
                crate::current_locale(&app_publish),
            );
            let answer =
                ask_solo_delivery_confirmation(&app_publish, &sess_publish, question, rationale)?;
            execute_solo_delivery_answer(answer, |confirmed| {
                let db_state = app_publish.state::<Db>();
                let conn = db_state.0.lock().map_err(|e| e.to_string())?;
                publish_local_run_inner(
                    &conn,
                    &sess_publish,
                    &run_publish,
                    repo_name,
                    private,
                    confirmed,
                )
            })
        }),
    }
}

// Security boundary: this is the complete agent-facing lead MCP surface. Undo must remain a
// user-initiated Tauri UI action and must never be exposed through lead tools.
pub(super) const LEAD_MCP_TOOL_NAMES: &[&str] = &[
    "dispatch_worker",
    "finish",
    "memory_set",
    "memory_add",
    "memory_read_source",
    "ask_user",
    "propose_verifier",
    "commit",
    "push",
    "create_pr",
    "publish",
];

pub(super) const LEAD_FINISH_DESCRIPTION: &str =
    "Call after all tasks are complete to declare the run finished. Parameters: evidence_refs(array, optional), rationale(string, optional).";
pub(super) const LEAD_MEMORY_SET_DESCRIPTION: &str = "Write an overwrite-slot memory record (the single current value, replacing the previous value in the same slot). Parameters: slot(string, required)=goal|state|next, text(string, required), title(string, optional; applies only to goal). Record the goal, state, and next step when wrapping up.";
pub(super) const LEAD_MEMORY_ADD_DESCRIPTION: &str = "Append a memory record as a new entry, optionally superseding older entries and including anchors. Parameters: category(string, required)=decision|pitfall|risk|watch, text(string, required), anchors(array, optional), supersedes(array of entry_id, optional), confidence(string, optional). Record key decisions, encountered pitfalls, risks, and open items.";
pub(super) const LEAD_MEMORY_READ_SOURCE_DESCRIPTION: &str = "Retrieve original transcript content by anchor (best effort; returns found:false instead of an error when not found). Parameters: anchor={kind:\"message\", ref:<message id>, block_index?, char_range?}, provided as either a single anchor object or an array of anchors. Use this when details from the original transcript are needed.";
pub(super) const LEAD_ASK_USER_DESCRIPTION: &str = "Call only in three situations: (1) an irreversible operation, (2) a scope change, or (3) a genuine user preference. Do not ask about operational decisions such as whether to redispatch a timed-out worker, retry strategy, or task ordering; decide autonomously and report briefly. After calling, wait for the user to select an option. If the user does not answer within the waiting window, the tool returns {status:\"pending_user\"} instead of blocking indefinitely. In that case, do not ask again or treat it as a failure; continue other work. The answer will later appear in your conversation context as a user message. Parameters: question(string, required)=the question, options(array of string, required, at least 2)=the options, recommended(string, optional)=the recommended option, rationale(string, optional)=why the question is being asked. A normal response is {answer: <the option selected by the user>}; a timed-out wait returns {status:\"pending_user\", note:<explanation>}.";
pub(super) const LEAD_PROPOSE_VERIFIER_DESCRIPTION: &str = "Run a verification command such as cargo test or npm test. Auto mode executes immediately without user confirmation, in place inside an offline sandbox, directly in the session worktree (the user's real project, including uncommitted changes and node_modules), and is expected not to modify the worktree. If it changes any tracked file content (including further rewriting or reverting existing uncommitted changes), creates an untracked file, or moves HEAD, verdict=failed and the affected files are reported accurately without automatic restoration. Writes to gitignored paths such as build caches are allowed. Use dispatch_worker for any file or code changes. The result (verdict/exit_code/output) is returned to the lead and also shown in the chat as a user-visible result card. Parameters: cmd(string, required)=the shell command to run, rationale(string, optional)=why verification is needed. Returns {ran:bool, verdict?:string, exit_code?:number, output?:string}.";
pub(super) const LEAD_COMMIT_DESCRIPTION: &str = "Safely commit the requested files. Each paths entry must be an existing individual file inside the worktree, or a file deleted from disk but still tracked in repository HEAD (to commit the deletion); directories and globs are not allowed. Gitignored files are rejected, except that a deleted path is checked against its registration in HEAD. Existing staged content from the user is preserved. Commit hooks are skipped. Before the first commit in a repository, the user sees the file list and must confirm; later commits in that repository do not require confirmation. Parameters: message(string, required), paths(array of string, required).";
pub(super) const LEAD_PUSH_DESCRIPTION: &str = "After user confirmation, push the current session's committed changes to origin. The tool rejects session changes that have not been committed. No parameters. It may return {\"status\":\"pending_user\"}, meaning nothing was executed during this call; call this tool again after the user confirms.";
pub(super) const LEAD_CREATE_PR_DESCRIPTION: &str = "After user confirmation, push the current session's committed changes and then create a GitHub Pull Request. The tool rejects session changes that have not been committed. Parameters: title(string, optional), body(string, optional). It may return {\"status\":\"pending_user\"}, meaning nothing was executed during this call; call this tool again after the user confirms.";
pub(super) const LEAD_PUBLISH_DESCRIPTION: &str = "After user confirmation, publish the committed local repository from a Local session as a new GitHub repository. The tool rejects session changes that have not been committed. Parameters: repo_name(string, optional), private(boolean, optional, default true). It may return {\"status\":\"pending_user\"}, meaning nothing was executed during this call; call this tool again after the user confirms.";
