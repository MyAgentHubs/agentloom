// This file contains report generation commands moved from lib.rs.

use crate::{db, new_run_id, project_files, repo_generation, worktree, Db};
use project_files::{read_project_file, repo_root_for_files};
use rusqlite::Connection;
use tauri::{AppHandle, Emitter, Manager, State};

#[derive(Clone, serde::Serialize)]
pub(super) struct GeneratedDocumentView {
    repo_id: String,
    content: String,
    generated_at: i64,
    head_sha: String,
    stale: bool,
}

#[derive(Clone, serde::Serialize)]
pub(super) struct GenerationRun {
    run_id: String,
}

#[derive(Clone, serde::Serialize)]
struct GenerationEvent<'a> {
    feature: &'a str,
    phase: &'a str,
    repo_id: &'a str,
    run_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    delta: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    document: Option<&'a db::GeneratedRepoDocument>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<&'a str>,
}

pub(super) fn emit_generation_event(
    app: &AppHandle,
    feature: &str,
    phase: &str,
    repo_id: &str,
    run_id: &str,
    delta: Option<&str>,
    document: Option<&db::GeneratedRepoDocument>,
    message: Option<&str>,
) {
    let _ = app.emit(
        "agent://event",
        GenerationEvent {
            feature,
            phase,
            repo_id,
            run_id,
            delta,
            document,
            message,
        },
    );
}

/// H1/A3: This does not use conn. The caller, start_repo_generation, has already resolved root to a
/// path, so these two file reads can happen outside the DB lock. The logic is identical to combining
/// read_repo_file_inner with read_project_file, except that it avoids the redundant repeated
/// repo_root_for_files query.
pub(super) fn optional_repo_material_at(root: &std::path::Path, path: &str) -> String {
    read_project_file(root, path)
        .map(|file| file.content)
        .unwrap_or_default()
}

pub(super) fn daily_session_material(conn: &Connection, repo_id: &str) -> Result<String, String> {
    let token_summary: (i64, i64) = conn
        .query_row(
            "SELECT COALESCE(SUM(total_input_tokens), 0), COALESCE(SUM(total_output_tokens), 0)
             FROM sessions WHERE repo_id = ?1",
            [repo_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|error| error.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT m.content FROM messages m
             JOIN sessions s ON s.id = m.session_id
             WHERE s.repo_id = ?1 AND m.role = 'assistant'
             ORDER BY m.created_at DESC LIMIT 10",
        )
        .map_err(|error| error.to_string())?;
    let messages = stmt
        .query_map([repo_id], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| error.to_string())?;
    Ok(format!(
        "累计 token：input={}，output={}\n近期 assistant 会话产出（JSON blocks）：\n{}",
        token_summary.0,
        token_summary.1,
        messages.join("\n")
    ))
}

pub(super) fn generation_prompt(
    feature: &str,
    readme: &str,
    claude_md: &str,
    commits: &str,
    sessions: &str,
) -> String {
    let request = if feature == "project_intro" {
        "只输出带 Markdown 小标题的四段：①项目是什么 ②技术栈 ③目录结构要点 ④最近在做什么。精炼、基于材料；可用 Read/Glob/Grep 补充核对，禁止修改任何文件。"
    } else {
        "输出 Markdown 日报，归纳：近期 commit、会话产出、待办要点、token 或 cost 概况（有则带，无则略）。精炼、基于材料；可用 Read/Glob/Grep 补充核对，禁止修改任何文件。"
    };
    format!(
        "{request}\n\nREADME:\n{readme}\n\nCLAUDE.md:\n{claude_md}\n\n近期 commits:\n{commits}\n\n会话与 token:\n{sessions}"
    )
}

fn start_repo_generation(
    app: AppHandle,
    repo_id: String,
    agent_id: String,
    feature: &'static str,
) -> Result<GenerationRun, String> {
    validate_generation_ids(&repo_id, &agent_id)?;
    let run_id = new_run_id();
    let root = {
        let db = app.state::<Db>();
        let conn = db.0.lock().map_err(|error| error.to_string())?;
        repo_root_for_files(&conn, &repo_id)?
    };
    let material = repo_generation::gather_repo_material(&root)?;
    let (command, parse_fn, stdin_prompt) = repo_generation::build_generation_command(
        &app, &repo_id, &agent_id, &run_id, feature, &root, &material,
    )?;

    emit_generation_event(
        &app, feature, "started", &repo_id, &run_id, None, None, None,
    );
    let app_t = app.clone();
    let repo_id_t = repo_id.clone();
    let run_id_t = run_id.clone();
    std::thread::spawn(move || {
        repo_generation::run_generation_child(
            app_t,
            repo_id_t,
            run_id_t,
            feature,
            command,
            parse_fn,
            stdin_prompt,
            material.head_sha,
        );
    });
    Ok(GenerationRun { run_id })
}

pub(super) fn validate_generation_ids(repo_id: &str, agent_id: &str) -> Result<(), String> {
    if repo_id.trim().is_empty() || agent_id.trim().is_empty() {
        Err("repo_id and agent_id are required".to_string())
    } else {
        Ok(())
    }
}

#[tauri::command]
pub(super) fn generate_project_intro(
    app: AppHandle,
    repo_id: String,
    agent_id: String,
) -> Result<GenerationRun, String> {
    start_repo_generation(app, repo_id, agent_id, "project_intro")
}

#[tauri::command]
pub(super) fn generate_daily(
    app: AppHandle,
    repo_id: String,
    agent_id: String,
) -> Result<GenerationRun, String> {
    start_repo_generation(app, repo_id, agent_id, "daily")
}

fn get_generated_document(
    conn: &Connection,
    repo_id: &str,
    daily: bool,
) -> Result<Option<GeneratedDocumentView>, String> {
    let document = if daily {
        db::get_daily_report(conn, repo_id)
    } else {
        db::get_project_intro(conn, repo_id)
    }
    .map_err(|error| error.to_string())?;
    let Some(document) = document else {
        return Ok(None);
    };
    let root = repo_root_for_files(conn, repo_id)?;
    let current_head = worktree::git_read_stdout_checked(&root, &["rev-parse", "HEAD"])?;
    Ok(Some(GeneratedDocumentView {
        stale: current_head.trim() != document.head_sha,
        repo_id: document.repo_id,
        content: document.content,
        generated_at: document.generated_at,
        head_sha: document.head_sha,
    }))
}

#[tauri::command]
pub(super) fn get_project_intro(
    db: State<Db>,
    repo_id: String,
) -> Result<Option<GeneratedDocumentView>, String> {
    let conn = db.0.lock().map_err(|error| error.to_string())?;
    get_generated_document(&conn, &repo_id, false)
}

#[tauri::command]
pub(super) fn get_daily(
    db: State<Db>,
    repo_id: String,
) -> Result<Option<GeneratedDocumentView>, String> {
    let conn = db.0.lock().map_err(|error| error.to_string())?;
    get_generated_document(&conn, &repo_id, true)
}
