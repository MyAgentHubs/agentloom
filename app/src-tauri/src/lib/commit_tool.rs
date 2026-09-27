use super::*;

fn parse_commit_args(
    args: &serde_json::Value,
) -> Result<(String, Vec<std::path::PathBuf>), String> {
    let message = args
        .get("message")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "commit: message must be a string".to_string())?
        .to_string();
    let paths = args
        .get("paths")
        .and_then(|value| value.as_array())
        .ok_or_else(|| "commit: paths must be an array of file paths".to_string())?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(std::path::PathBuf::from)
                .ok_or_else(|| "commit: every paths entry must be a string".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok((message, paths))
}

fn confirm_commit_if_needed(
    app: &AppHandle,
    session_id: &str,
    worktree: &std::path::Path,
    paths: &[std::path::PathBuf],
    repo_key: &str,
) -> Result<bool, String> {
    let selection = commit_broker::compute_committable_selection(worktree, paths)?;
    let locale = crate::current_locale(app);
    let confirmation_args =
        lead_commit_confirmation_args(format_lead_commit_preview(&selection, locale), locale);
    let (confirm_label, cancel_label) =
        confirmation_option_labels(&confirmation_args, "commit confirmation")?;

    let answer = lead_tools::ask_user(
        app,
        session_id,
        confirmation_args,
        // The commit tool is shared by solo and lead, with no natural identity source here;
        // preserve the existing behavior (`None`).
        None,
        None,
    )?
    .get("answer")
    .and_then(|value| value.as_str())
    .ok_or_else(|| "commit: confirmation returned no answer".to_string())?
    .to_string();
    if lead_commit_confirmation_is_cancelled(&answer, &confirm_label, &cancel_label)? {
        return Ok(true);
    }

    let db_state = app.state::<Db>();
    let conn = db_state.0.lock().map_err(|e| e.to_string())?;
    // Repository-scoped authorization is intentionally shared by every
    // session and agent using this worktree after the user's first approval.
    db::set_commit_authorized(&conn, repo_key, true)?;
    Ok(false)
}

fn record_commit_result(
    app: &AppHandle,
    worktree: &std::path::Path,
    session_id: &str,
    run_id: &str,
    expected_head: &str,
    run_pre_head: &str,
    result: Result<commit_broker::CommitResult, String>,
) -> Result<serde_json::Value, String> {
    match result {
        Err(error) => {
            let current_head = worktree::rev_parse_head(worktree).unwrap_or_default();
            let db_state = app.state::<Db>();
            let conn = db_state.0.lock().map_err(|e| e.to_string())?;
            if current_head == expected_head {
                db::delete_run_commit_intent(&conn, session_id, run_id)
                    .map_err(|e| e.to_string())?;
            } else {
                let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
                db::mark_run_failed(&conn, session_id, run_id).map_err(|e| e.to_string())?;
                db::set_git_state(&conn, session_id, "commit_failed").map_err(|e| e.to_string())?;
                tx.commit().map_err(|e| e.to_string())?;
                return Err(format!(
                    "{error}; repository HEAD changed to {current_head}, so the commit result is ambiguous and requires reconciliation"
                ));
            }
            Err(error)
        }
        Ok(commit_broker::CommitResult::Committed {
            sha,
            committed_paths,
        }) => {
            let db_state = app.state::<Db>();
            let conn = db_state.0.lock().map_err(|e| e.to_string())?;
            let warning =
                finish_commit_ledger(&conn, worktree, session_id, run_id, run_pre_head, &sha)?;
            Ok(serde_json::json!({
                "sha": sha,
                "committed": committed_paths
                    .into_iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect::<Vec<_>>(),
                "dropped": Vec::<serde_json::Value>::new(),
                "ledger_warning": warning,
            }))
        }
        Ok(commit_broker::CommitResult::Refused { reason }) => {
            let db_state = app.state::<Db>();
            let conn = db_state.0.lock().map_err(|e| e.to_string())?;
            db::delete_run_commit_intent(&conn, session_id, run_id).map_err(|e| e.to_string())?;
            Ok(serde_json::json!({
                "refused": reason,
                "dropped": Vec::<serde_json::Value>::new(),
            }))
        }
    }
}

pub(crate) fn build_commit_tool(
    app: &AppHandle,
    session_id: &str,
    run_id: &str,
    worktree: &std::path::Path,
) -> mcp_server::ToolDef {
    let app_commit = app.clone();
    let sess_commit = session_id.to_string();
    let run_commit = run_id.to_string();
    let wt_commit = worktree.to_path_buf();

    mcp_server::ToolDef {
        name: "commit".to_string(),
        description: LEAD_COMMIT_DESCRIPTION.to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "message": {"type": "string"},
                "paths": {"type": "array", "items": {"type": "string"}}
            },
            "required": ["message", "paths"]
        }),
        handler: Box::new(move |args: serde_json::Value| {
            let (message, paths) = parse_commit_args(&args)?;

            let canonical_worktree = std::fs::canonicalize(&wt_commit)
                .map_err(|e| format!("规范化 worktree 路径失败: {e}"))?;
            let repo_key = canonical_worktree.to_string_lossy().into_owned();
            let app_data_dir = app_commit
                .path()
                .app_data_dir()
                .map_err(|e| format!("解析 app_data_dir 失败(拒绝在无 app 域读保护下提交): {e}"))?;
            let authorized = {
                let db_state = app_commit.state::<Db>();
                let conn = db_state.0.lock().map_err(|e| e.to_string())?;
                let _store = checkpoint::CheckpointStore::new(&conn)?;
                db::is_commit_authorized(&conn, &repo_key)?
            };

            if lead_commit_requires_preview(authorized)
                && confirm_commit_if_needed(
                    &app_commit,
                    &sess_commit,
                    &wt_commit,
                    &paths,
                    &repo_key,
                )?
            {
                return Ok(serde_json::json!({"refused": "user cancelled"}));
            }

            let run = {
                let db_state = app_commit.state::<Db>();
                let conn = db_state.0.lock().map_err(|e| e.to_string())?;
                db::run_commit(&conn, &sess_commit, &run_commit)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "commit: current run ledger is missing".to_string())?
            };
            let expected_head = run.post_head.as_deref().unwrap_or(&run.pre_head);
            let current_head = worktree::rev_parse_head(&wt_commit)
                .map_err(|e| format!("commit: cannot read current HEAD: {e}"))?;
            if current_head != expected_head {
                return Err(format!(
                    "commit: repository HEAD changed outside this run (expected {expected_head}, found {current_head})"
                ));
            }
            {
                let db_state = app_commit.state::<Db>();
                let conn = db_state.0.lock().map_err(|e| e.to_string())?;
                db::begin_run_commit_intent(
                    &conn,
                    &sess_commit,
                    &run_commit,
                    expected_head,
                    &run.state,
                )
                .map_err(|e| e.to_string())?;
            }

            let result = commit_broker::mediate_commit_for_session(
                &wt_commit,
                Some(app_data_dir.as_path()),
                &message,
                &paths,
                true,
            );

            record_commit_result(
                &app_commit,
                &wt_commit,
                &sess_commit,
                &run_commit,
                expected_head,
                &run.pre_head,
                result,
            )
        }),
    }
}
