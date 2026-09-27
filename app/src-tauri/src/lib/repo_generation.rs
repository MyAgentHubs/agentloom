use super::*;

pub(super) struct RepoMaterial {
    pub(super) head_sha: String,
    commits: String,
    readme: String,
    claude_md: String,
}

pub(super) fn gather_repo_material(root: &std::path::Path) -> Result<RepoMaterial, String> {
    let head_sha = worktree::git_read_stdout_checked(root, &["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    let commits = worktree::git_read_stdout_checked(
        root,
        &[
            "log",
            "-n",
            "20",
            "--date=short",
            "--pretty=format:%h %ad %s",
        ],
    )?;
    let readme = optional_repo_material_at(root, "README.md");
    let claude_md = optional_repo_material_at(root, "CLAUDE.md");
    Ok(RepoMaterial {
        head_sha,
        commits,
        readme,
        claude_md,
    })
}

pub(super) fn build_generation_command(
    app: &AppHandle,
    repo_id: &str,
    agent_id: &str,
    run_id: &str,
    feature: &'static str,
    root: &std::path::Path,
    material: &RepoMaterial,
) -> Result<(Command, ParseFn, Option<agent::StdinPrompt>), String> {
    let db = app.state::<Db>();
    let (prompt, profile) = {
        let conn = db.0.lock().map_err(|error| error.to_string())?;
        let sessions = if feature == "daily" {
            daily_session_material(&conn, repo_id)?
        } else {
            String::new()
        };
        let prompt = generation_prompt(
            feature,
            &material.readme,
            &material.claude_md,
            &material.commits,
            &sessions,
        );
        let profile = db::get_agent(&conn, agent_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| ui_msg::al_err("agent.notFound", &[]))?;
        (prompt, profile)
    };
    let search =
        resolve_harness_search_creds(db.inner(), &profile, &crate::keychain::KeyringStore)?;
    let key = resolve_member_key(&profile)?;
    let (command, parse_fn, stdin_prompt) = {
        let conn = db.0.lock().map_err(|error| error.to_string())?;
        build_lead_backend_command(
            &conn,
            &format!("repo-summary-{repo_id}"),
            run_id,
            &profile,
            &prompt,
            root,
            agent::BuildMode::Summarize,
            current_locale(app),
            None,
            key,
            search,
        )?
    };
    Ok((command, parse_fn, stdin_prompt))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_generation_child(
    app: AppHandle,
    repo_id: String,
    run_id: String,
    feature: &'static str,
    mut command: Command,
    parse_fn: ParseFn,
    stdin_prompt: Option<agent::StdinPrompt>,
    head_sha: String,
) {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let result = (|| -> Result<db::GeneratedRepoDocument, String> {
        let mut child = agent::spawn_with_stdin_prompt(&mut command, stdin_prompt.as_ref())
            .map_err(|error| error.to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "agent stdout unavailable".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "agent stderr unavailable".to_string())?;
        let stderr_reader = std::thread::spawn(move || {
            let mut stderr = stderr;
            let mut bytes = Vec::new();
            let _ = stderr.read_to_end(&mut bytes);
            bytes
        });
        let mut content = String::new();
        for line in BufReader::new(stdout).lines() {
            let line = line.map_err(|error| error.to_string())?;
            for event in parse_agent_line_for_locale(parse_fn, &line, current_locale(&app)) {
                match event {
                    agent_event::AgentEvent::TextDelta { text } => {
                        content.push_str(&text);
                        emit_generation_event(
                            &app,
                            feature,
                            "delta",
                            &repo_id,
                            &run_id,
                            Some(&text),
                            None,
                            None,
                        );
                    }
                    agent_event::AgentEvent::Completed {
                        final_text: Some(text),
                        ..
                    } => {
                        if content.trim().is_empty() {
                            content = text;
                        }
                    }
                    agent_event::AgentEvent::Error { message }
                    | agent_event::AgentEvent::Blocked { message, .. } => return Err(message),
                    _ => {}
                }
            }
        }
        let status = child.wait().map_err(|error| error.to_string())?;
        let stderr = stderr_reader.join().unwrap_or_default();
        if !status.success() {
            return Err(String::from_utf8_lossy(&stderr).trim().to_string());
        }
        if content.trim().is_empty() {
            return Err("agent returned no text".to_string());
        }
        let document = db::GeneratedRepoDocument {
            repo_id: repo_id.clone(),
            content,
            generated_at: db::now_secs(),
            head_sha,
        };
        let state = app.state::<Db>();
        let conn = state.0.lock().map_err(|error| error.to_string())?;
        if feature == "project_intro" {
            db::upsert_project_intro(&conn, &document)
        } else {
            db::upsert_daily_report(&conn, &document)
        }
        .map_err(|error| error.to_string())?;
        Ok(document)
    })();
    match result {
        Ok(document) => emit_generation_event(
            &app,
            feature,
            "completed",
            &repo_id,
            &run_id,
            None,
            Some(&document),
            None,
        ),
        Err(message) => emit_generation_event(
            &app,
            feature,
            "error",
            &repo_id,
            &run_id,
            None,
            None,
            Some(&message),
        ),
    }
}
