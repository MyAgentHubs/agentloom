use super::*;

pub(crate) fn parser_for_parse_fn(parse_fn: ParseFn) -> fn(&str) -> Vec<agent_event::AgentEvent> {
    match parse_fn {
        ParseFn::Claude => agent_event::parse_claude_line,
        ParseFn::Codex => agent_event::parse_codex_line,
        ParseFn::Harness => agent_event::parse_harness_line,
        ParseFn::HarnessPlan => agent_event::parse_harness_plan_line,
    }
}

pub(crate) fn parse_agent_line_for_locale(
    parse_fn: ParseFn,
    line: &str,
    locale: Locale,
) -> Vec<agent_event::AgentEvent> {
    match parse_fn {
        ParseFn::Claude => agent_event::parse_claude_line_for_locale(line, locale),
        ParseFn::Codex => agent_event::parse_codex_line_for_locale(line, locale),
        ParseFn::Harness => agent_event::parse_harness_line_for_locale(line, locale),
        ParseFn::HarnessPlan => agent_event::parse_harness_plan_line_for_locale(line, locale),
    }
}

pub(crate) fn parse_fn_for_profile(profile: &db::AgentProfile) -> ParseFn {
    if profile.access == "native" && profile.provider == "codex" {
        ParseFn::Codex
    } else if profile.access == "harness" && agent::harness_plan_mode_enabled() {
        ParseFn::HarnessPlan
    } else if profile.access == "harness" {
        ParseFn::Harness
    } else {
        ParseFn::Claude
    }
}

pub(crate) fn codex_thread_id_from_event(
    parse_fn: ParseFn,
    event: &agent_event::AgentEvent,
) -> Option<&str> {
    if !matches!(parse_fn, ParseFn::Codex) {
        return None;
    }
    match event {
        agent_event::AgentEvent::SessionStarted { conversation_id } => Some(conversation_id),
        _ => None,
    }
}

pub(crate) fn scan_new_images(
    dir: &std::path::Path,
    since: std::time::SystemTime,
) -> Vec<std::path::PathBuf> {
    const IMAGE_LIMIT: usize = 20;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut images: Vec<_> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let extension = path.extension()?.to_str()?.to_ascii_lowercase();
            if !matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp") {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            if !metadata.is_file() || metadata.modified().ok()? < since {
                return None;
            }
            Some(path)
        })
        .collect();
    images.sort();
    images.truncate(IMAGE_LIMIT);
    images
}

pub(crate) fn codex_generated_images_dir(thread_id: &str) -> Option<std::path::PathBuf> {
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home_dir_for_attachment().join(".codex"));
    let codex_home = if codex_home.is_absolute() {
        codex_home
    } else {
        std::env::current_dir().ok()?.join(codex_home)
    };
    Some(codex_home.join("generated_images").join(thread_id))
}

pub(crate) fn codex_image_tool_events(
    run_id: &str,
    images: &[std::path::PathBuf],
) -> [agent_event::AgentEvent; 2] {
    let id = format!("codex-image-{run_id}");
    let output = images
        .iter()
        .map(|path| path.to_string_lossy())
        .collect::<Vec<_>>()
        .join("\n");
    [
        agent_event::AgentEvent::ToolStarted {
            id: id.clone(),
            tool: "image_gen".to_string(),
            summary: format!("Generated {} image(s)", images.len()),
            card: agent_event::CardKind::Compact,
        },
        agent_event::AgentEvent::ToolCompleted {
            id,
            status: agent_event::ToolStatus::Ok,
            exit_code: None,
            output: Some(output),
        },
    ]
}

/// The caller must pre-resolve `key` / `search` without holding the `db.0` lock before passing them to this function. Parameters and
/// return values are unchanged on the normal path; on the rare path where reacquiring the final lock fails, agent key IPC now happens first (previously it did not happen when lock acquisition failed).
pub(crate) fn build_lead_backend_command(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
    profile: &db::AgentProfile,
    prompt: &str,
    wt: &std::path::Path,
    mode: agent::BuildMode,
    locale: Locale,
    reasoning_tier: Option<&str>,
    key: Option<String>,
    search: HarnessSearchCreds,
) -> Result<(Command, ParseFn, Option<agent::StdinPrompt>), String> {
    let backend = make_backend(profile, key, search, locale)?;
    let parse_fn = backend.parse_fn();
    let ctx = agent::BuildContext {
        prompt,
        session_id,
        run_id,
        wt,
        conn,
        mode,
        locale,
        reasoning_tier,
        criteria: &[],
    };
    let command = backend.build_command(&ctx)?;
    let stdin_prompt = backend.stdin_prompt(&ctx);
    Ok((command, parse_fn, stdin_prompt))
}

#[allow(dead_code)] // Reference implementation that invokes the full profile→key→wt→command flow at once; production code now entirely
                    // uses staged helper functions (`prepare_team_members` in `start_team_run`,
                    // and `run_single_worker` both call get_member_agent_profile/resolve_member_key/
                    // build_member_command_with directly); this remains as a field-by-field equivalence reference for tests
                    // (see split_helpers_recombine_to_the_same_command_as_build_member_command)
                    // and existing tests such as team_members_share_the_same_bound_project_cwd.
type BuiltMemberCommand = (
    Command,
    fn(&str) -> Vec<agent_event::AgentEvent>,
    ParseFn,
    std::path::PathBuf,
    member_runner::TextGranularity,
    Option<agent::StdinPrompt>,
);

/// Build (command, parser, parse_fn, cwd, returned-text accumulation granularity) for one member: seam 4 through make_backend/member.
/// Members in the same session share the user's project cwd; granularity and parser derive from the same source (`TextGranularity::for_parse_fn`).
/// Internally this follows "read DB profile → keychain/build workspace (slow, no conn needed) → assemble Command (needs conn, but fast)"
/// in three stages (see the three pub(crate) helpers below); this function chains all three at once. **State after the H1 follow-up**:
/// Both production paths, `start_team_run` (via `prepare_team_members`) and `run_single_worker`, now
/// call the three helpers in stages directly (moving the two slow operations, keychain IPC and git worktree, outside the global DB lock) and no longer call
/// this all-at-once version—it remains only as a reference implementation for tests (`#[allow(dead_code)]`, with existing precedents in this repository,
/// such as `run_single_worker`'s own `#[allow(dead_code)]`).
/// Internally it still directly executes profile→key→search→wt→command at once, intentionally preserving the in-lock resolution anti-pattern that RN4-a removed from four lead call sites
/// as an equivalence baseline, not as a production recommendation; new code should follow
/// `build_member_command_with` or the three-stage pattern at those four lead call sites, not this function.
#[allow(dead_code)]
pub(crate) fn build_member_command(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
    spec: &member_runner::MemberSpec,
    locale: Locale,
) -> Result<BuiltMemberCommand, String> {
    // Execution-order record (added after Opus adversarial review F4-1): before H1 it was profile → key → make_backend → wt
    // (workspace creation came after make_backend); now it is profile → key → wt → make_backend
    // (workspace creation moved before build_member_command_with because build_member_command_with
    // couples make_backend with build_command, while wt is a required build_command input). The normal-path
    // result is unchanged, but failure-path side-effect order changed: if make_backend errors (for example, access is "borrow" but
    // no key is configured), this member's git worktree (for a non-in-place session) is now created before the error—
    // leaving one additional residual worktree (previously make_backend failed first and wt was never created). This residue is harmless
    // (the next preparation for the same assignment_id reuses/cleans it; this is not data corruption), but it is a new side-effect
    // order introduced by this change and is recorded here explicitly.
    let profile = get_member_agent_profile(conn, &spec.agent_id)?;
    let key = resolve_member_key(&profile)?;
    let search = if profile.access == "harness" {
        let backend = active_search_backend_name(conn);
        let key = resolve_search_key(&crate::keychain::KeyringStore, &backend);
        HarnessSearchCreds {
            key,
            backend: Some(backend),
        }
    } else {
        HarnessSearchCreds::default()
    };
    let wt = resolve_member_wt(conn, session_id, &spec.assignment_id)?;
    let (command, parser, parse_fn, granularity, stdin_prompt) = build_member_command_with(
        conn, session_id, run_id, spec, &profile, key, search, &wt, locale,
    )?;
    Ok((command, parser, parse_fn, wt, granularity, stdin_prompt))
}

/// build_member_command stage 1: read the agent profile from the DB (fast; requires conn).
pub(crate) fn get_member_agent_profile(
    conn: &rusqlite::Connection,
    agent_id: &str,
) -> Result<db::AgentProfile, String> {
    db::get_agent(conn, agent_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| ui_msg::al_err("agent.notFound", &[]))
}

/// build_member_command stage 2a: keychain IPC (slow; does not require conn and can run after releasing the DB lock).
pub(crate) fn resolve_member_key(profile: &db::AgentProfile) -> Result<Option<String>, String> {
    if profile.access == "borrow" || profile.access == "harness" {
        KeyringStore.get(&profile.id)
    } else {
        Ok(None)
    }
}

/// build_member_command stage 3: assemble the final Command from pre-resolved profile/key/search/wt (requires conn).
/// **F3-1 historical correction (revised after Opus adversarial review)**: before the refactor, the "harness" branch of `make_backend` called
/// `resolve_harness_search` → `keychain::get_search_key_with_store`, performing real keychain IPC while holding the lock;
/// it fetched the search backend API key, which is distinct from the agent's own key fetched by `resolve_member_key`.
/// T5d-b.1-RN3-b added `HarnessSearchCreds` + `resolve_harness_search_creds`, moving only search-key resolution
/// outside the lock; the inline resolution of the agent's own key in `build_lead_backend_command` still remained inside the lock then. RN4-a subsequently moved this
/// remaining half outside the lock; the four call sites `start_repo_generation` / `propose_team_plan` / `lead_step` /
/// `generate_handoff_doc` now resolve the agent key and search credentials before acquiring the final Command-building lock.
/// The solo continuation branch of `start_continuation_session` previously performed two IPC calls inside the lock via `build_send_plan`;
/// RN4-a switched it to `build_send_plan_with`; `build_send_plan` has had no production callers since and is now `#[cfg(test)]`.
///
/// Two follow-ups remain: 1. all harness members in the same team run still resolve search credentials independently per member;
/// sharing one set resolved only once is outside this change's scope; 2. `start_lead_session` still has a direct call to `resolve_harness_search`
/// that bypasses `make_backend` and still occurs while holding `db.0.lock()`. It is a related call site identified in this review
/// but is not one of the four direct call sites above; it remains unchanged for a separate follow-up.
pub(crate) fn build_member_command_with(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
    spec: &member_runner::MemberSpec,
    profile: &db::AgentProfile,
    key: Option<String>,
    search: HarnessSearchCreds,
    wt: &std::path::Path,
    locale: Locale,
) -> Result<
    (
        Command,
        fn(&str) -> Vec<agent_event::AgentEvent>,
        ParseFn,
        member_runner::TextGranularity,
        Option<agent::StdinPrompt>,
    ),
    String,
> {
    let backend = make_backend(profile, key, search, locale)?;
    let ctx = agent::BuildContext {
        prompt: &spec.prompt,
        session_id,
        run_id,
        wt,
        conn,
        mode: agent::BuildMode::Worker,
        locale,
        reasoning_tier: None,
        criteria: &[],
    };
    let command = backend.build_command(&ctx)?;
    let stdin_prompt = backend.stdin_prompt(&ctx);
    let parse_fn = backend.parse_fn();
    let parser = parser_for_parse_fn(parse_fn);
    let granularity = member_runner::TextGranularity::for_parse_fn(parse_fn);
    Ok((command, parser, parse_fn, granularity, stdin_prompt))
}

pub(crate) struct SendPlan {
    pub(crate) profile: db::AgentProfile,
    pub(crate) agent_id: String,
    pub(crate) name_snapshot: String,
    pub(crate) prompt: String,
    pub(crate) wt: std::path::PathBuf,
    pub(crate) command: Command,
    pub(crate) parse_fn: ParseFn,
    pub(crate) stdin_prompt: Option<agent::StdinPrompt>,
}

pub(crate) fn build_send_plan_with(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
    profile: db::AgentProfile,
    key: Option<String>,
    search: HarnessSearchCreds,
    message: &str,
    reasoning_tier: Option<&str>,
    criteria: &[String],
    locale: Locale,
) -> Result<SendPlan, String> {
    let plan_agent_id = profile.id.clone();
    let name_snapshot = profile.name.clone();
    let backend = make_backend(&profile, key, search, locale)?;
    let prompt = if profile.access == "harness" && agent::harness_plan_mode_enabled() {
        build_agent_prompt(&profile, &[], message, locale, None, None)
    } else {
        let history = db::get_messages(conn, session_id).map_err(|e| e.to_string())?;
        let compact_state = if profile.access == "harness" {
            db::get_compact_state(conn, session_id).map_err(|e| e.to_string())?
        } else {
            None
        };
        let transcript_nonce =
            (profile.access == "harness").then(|| uuid::Uuid::new_v4().simple().to_string());
        build_agent_prompt(
            &profile,
            &history,
            message,
            locale,
            compact_state.as_ref(),
            transcript_nonce.as_deref(),
        )
    };
    let (_, wt) = ensure_session_workspace(conn, session_id)?;
    let parse_fn = backend.parse_fn();
    let ctx = BuildContext {
        prompt: &prompt,
        session_id,
        run_id,
        wt: &wt,
        conn,
        mode: agent::BuildMode::Normal,
        locale,
        reasoning_tier,
        criteria,
    };
    let command = backend.build_command(&ctx)?;
    let stdin_prompt = backend.stdin_prompt(&ctx);

    Ok(SendPlan {
        profile,
        agent_id: plan_agent_id,
        name_snapshot,
        prompt,
        wt,
        command,
        parse_fn,
        stdin_prompt,
    })
}

/// **Test baseline; in-lock IPC anti-pattern; disabled in production**: retain the original all-at-once path for equivalence tests.
#[cfg(test)]
pub(crate) fn build_send_plan(
    conn: &rusqlite::Connection,
    session_id: &str,
    run_id: &str,
    agent_id: &str,
    message: &str,
    reasoning_tier: Option<&str>,
    criteria: &[String],
    key_store: &dyn KeyStore,
    locale: Locale,
) -> Result<SendPlan, String> {
    let profile = db::get_agent(conn, agent_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| ui_msg::al_err("agent.notFound", &[]))?;
    let key = if profile.access == "borrow" || profile.access == "harness" {
        key_store.get(&profile.id)?
    } else {
        None
    };
    let search = if profile.access == "harness" {
        let backend = active_search_backend_name(conn);
        let search_key = resolve_search_key(key_store, &backend);
        HarnessSearchCreds {
            key: search_key,
            backend: Some(backend),
        }
    } else {
        HarnessSearchCreds::default()
    };
    build_send_plan_with(
        conn,
        session_id,
        run_id,
        profile,
        key,
        search,
        message,
        reasoning_tier,
        criteria,
        locale,
    )
}

pub(crate) fn require_agent_id(agent_id: String) -> Result<String, String> {
    if agent_id.is_empty() {
        Err(ui_msg::al_err("agent.missingId", &[]))
    } else {
        Ok(agent_id)
    }
}

pub(crate) fn session_continued_readonly_message(locale: Locale) -> &'static str {
    match locale {
        Locale::Zh => "会话已交接到新会话·只读·请到新会话继续",
        Locale::En => {
            "Session handed off to a new session · read-only · continue in the new session"
        }
    }
}

pub(crate) fn ensure_session_not_continued(
    conn: &rusqlite::Connection,
    session_id: &str,
    locale: Locale,
) -> Result<(), String> {
    let continued_to_session_id: Option<String> = conn
        .query_row(
            "SELECT continued_to_session_id FROM sessions WHERE id = ?1",
            [session_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .flatten();
    if continued_to_session_id.is_some() {
        return Err(session_continued_readonly_message(locale).to_string());
    }
    if db::session_has_live_children(conn, session_id).map_err(|e| e.to_string())? {
        return Err(session_continued_readonly_message(locale).to_string());
    }
    Ok(())
}

pub(crate) fn normalize_reasoning_tier(
    reasoning_tier: Option<String>,
) -> Result<Option<String>, String> {
    let Some(tier) = reasoning_tier else {
        return Ok(None);
    };
    let tier = tier.trim().to_ascii_lowercase();
    match tier.as_str() {
        "auto" => Ok(Some("medium".to_string())),
        "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" => Ok(Some(tier)),
        _ => Err(ui_msg::al_err(
            "agent.invalidReasoningTier",
            &[("tier", tier)],
        )),
    }
}
