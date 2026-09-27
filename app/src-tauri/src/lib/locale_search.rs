use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Locale {
    #[default]
    Zh,
    En,
}

impl Locale {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "zh" => Some(Self::Zh),
            "en" => Some(Self::En),
            _ => None,
        }
    }
}

#[derive(Default)]
pub(crate) struct UiLocale(pub(crate) RwLock<Locale>);

#[tauri::command]
pub(crate) fn set_ui_locale(state: State<'_, UiLocale>, locale: String) -> Result<(), String> {
    let locale = Locale::parse(&locale).ok_or_else(|| ui_msg::al_err("ui.badLocale", &[]))?;
    *state
        .0
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = locale;
    Ok(())
}

pub(crate) fn current_locale(app: &AppHandle) -> Locale {
    app.try_state::<UiLocale>()
        .map(|state| {
            *state
                .0
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        })
        .unwrap_or_default()
}

pub(crate) fn validate_criteria(lines: &[String]) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for raw in lines {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line.len() > MAX_CRITERION_LEN {
            return Err(ui_msg::al_err(
                "criteria.lineTooLong",
                &[("max", MAX_CRITERION_LEN.to_string())],
            ));
        }
        let ok = if let Some(rest) = line.strip_prefix("cmd:") {
            !rest.trim().is_empty()
        } else if let Some(rest) = line.strip_prefix("contains:") {
            match rest.split_once(':') {
                Some((needle, cmd)) => !needle.trim().is_empty() && !cmd.trim().is_empty(),
                None => false,
            }
        } else if let Some(rest) = line.strip_prefix("judge:") {
            !rest.trim().is_empty()
        } else {
            false
        };
        if !ok {
            return Err(ui_msg::al_err(
                "criteria.invalidSyntax",
                &[("raw", raw.clone())],
            ));
        }
        out.push(line.to_string());
    }
    if out.len() > MAX_CRITERIA {
        return Err(ui_msg::al_err(
            "criteria.tooMany",
            &[("max", MAX_CRITERIA.to_string())],
        ));
    }
    Ok(out)
}

pub(crate) fn language_directive(locale: Locale) -> &'static str {
    match locale {
        Locale::Zh => "\n\n语言要求：用用户最新一条消息所用的语言回复（用户用英文提问就用英文回复，用中文提问就用中文回复）。消息语言不明或中英混杂时，用中文回复。",
        Locale::En => "\n\nLanguage: reply in the SAME language as the user's latest message (English question gets an English reply; Chinese question gets a Chinese reply). If the message language is unclear or mixed, reply in English.",
    }
}

pub(crate) fn build_prompt(
    history: &[db::Message],
    current: &str,
    locale: Locale,
    compact_state: Option<&db::CompactState>,
    transcript_nonce: Option<&str>,
) -> String {
    if history.is_empty() {
        return current.to_string();
    }
    // T7a: marker mode also includes this preamble—the parser collects text before the first marker verbatim into the preamble
    // and restores it byte-for-byte when rendering, so both modes share the same sentence instead of leaving marker mode with a bare opening.
    let mut s = String::from(match locale {
        Locale::Zh => "以下是我们之前的对话历史：\n\n",
        Locale::En => "Here is our previous conversation history:\n\n",
    });
    if let (Some(compact), Some(nonce)) = (compact_state, transcript_nonce) {
        // T7a M-2: do not render the summary section when the summary is empty; the compact boundary remains valid, and old messages continue to be filtered by
        // through_message_id to avoid putting already-covered history back into the prompt.
        if !compact.summary.is_empty() {
            s.push_str(&format!(
                "===== AGENTLOOM-COMPACT-SUMMARY {nonce} through={} =====\n",
                compact.through_message_id
            ));
            s.push_str(&compact.summary);
            if !compact.summary.ends_with('\n') {
                s.push('\n');
            }
            s.push_str(&format!("===== /AGENTLOOM-COMPACT-SUMMARY {nonce} =====\n"));
        }
    }
    let mut rendered_messages = 0;
    for m in history.iter().filter(|message| {
        compact_state
            .filter(|_| transcript_nonce.is_some())
            .is_none_or(|compact| message.id > compact.through_message_id)
    }) {
        if let Some(nonce) = transcript_nonce {
            s.push_str(&format!(
                "===== AGENTLOOM-MSG {nonce} id={} role={} =====\n",
                m.id, m.role
            ));
        }
        let (who, separator): (&str, &str) = match (locale, m.role.as_str()) {
            (Locale::Zh, "user") => ("用户", "："),
            (Locale::Zh, _) => ("助手", "："),
            (Locale::En, "user") => ("User", ": "),
            (Locale::En, _) => ("Assistant", ": "),
        };
        s.push_str(who);
        s.push_str(separator);
        s.push_str(&db::blocks_to_text(&m.content));
        s.push_str("\n\n");
        rendered_messages += 1;
    }
    if let Some(nonce) = transcript_nonce {
        if compact_state.is_some() || rendered_messages > 0 {
            s.push_str(&format!("===== AGENTLOOM-HISTORY-END {nonce} =====\n\n"));
        }
    }
    s.push_str(match locale {
        Locale::Zh => "请基于以上历史，自然地继续回答用户最新的消息：\n\n用户：",
        Locale::En => "Please continue naturally, answering the user's latest message based on the history above:\n\nUser: ",
    });
    s.push_str(current);
    s.push_str(language_directive(locale));
    s
}

pub(crate) fn build_agent_prompt(
    profile: &db::AgentProfile,
    history: &[db::Message],
    current: &str,
    locale: Locale,
    compact_state: Option<&db::CompactState>,
    transcript_nonce: Option<&str>,
) -> String {
    if profile.access == "harness" && agent::harness_plan_mode_enabled() {
        current.to_string()
    } else if profile.access == "harness" {
        build_prompt(history, current, locale, compact_state, transcript_nonce)
    } else {
        build_prompt(history, current, locale, None, None)
    }
}

pub(crate) fn build_synthesis_prompt(goal: &str, workers: &[(String, String)]) -> String {
    let mut s = String::from(
        "You are the Agent Team's lead synthesis writer. Your task is to synthesize the outputs of multiple workers into a professional, deliverable report-style response.\n\
         Only synthesize and summarize; do not modify any files, run commands, or introduce new facts.\n\
         \n\
         Output requirements:\n\
         - Lead with conclusions: before any ## section, provide 2-4 executive-summary bullets. Do not use labels such as “给老板的一句话”, “TL;DR for the Boss”, or “老板”.\n\
         - Organize by topic: then use markdown level-two headings (## Heading) to divide the response into 3-6 topical sections; do not organize sections by worker; do not use a # level-one heading; avoid ### in the body whenever possible.\n\
         - Keep ## headings short, do not present Chinese and English headings side by side, and avoid exceeding 4 English words or 12 Chinese characters.\n\
         - The first line of every ## section must identify the source workers in this format: **Synthesized from:** Worker A, Worker B (for a Chinese target language, use **综合自：** 队员 A、队员 B).\n\
         - In each section, give the judgment first and then the supporting basis. Do not merely list the workers' original text.\n\
         - When workers' conclusions, facts, or recommendations disagree, explicitly list the conflicts and explain which points still require verification.\n\
         - Content unsupported by any worker, lacking sufficient evidence, or inferred by the lead must be labeled “Unverified” (for a Chinese target language, label it “未验证”).\n\
         - Preserve the original text of code, file names, commands, APIs, and proper nouns.\n\
         - Preserve Markdown image references exactly as written, including `![alt](path)` syntax and bare image paths; never rewrite or omit them.\n\
         - Follow the natural language of the “Goal” below: use Chinese for a Chinese goal and English for an English goal; when workers' output languages differ, normalize them to the goal's language; do not force English output merely because this prompt is in English. Use an overall bilingual format only when the “Goal” explicitly requests Chinese-English side-by-side or bilingual output; otherwise, only parenthetically annotate a term in the other language when it first appears in the body (for an English target language, for example, “capital markets（资本市场）”; for a Chinese target language, for example, “资本市场（capital markets）”), and do not make headings or table headers bilingual.\n\
         \n\
         Table usage rules:\n\
         - When the content naturally suits horizontal comparison, side-by-side evaluation, or structured delivery, prefer markdown GFM tables (such as solution comparisons, risk lists, regional comparisons, file-change lists, evidence strength, or next actions).\n\
         - Do not force a table merely to make the response look like a report; when a single path, a small number of facts, or a narrative is clearer, use concise paragraphs or a bullet list.\n\
         - Keep tables to 3-5 columns; use the goal's language for column names; use short phrases in cells; include columns such as “Basis/Evidence”, “Impact”, “Recommended Action”, and “Status” when needed.\n\
         \n\
         Tone requirements:\n\
         - Use professional written language, like a synthesis report delivered to a product, engineering, or business team.\n\
         - Avoid colloquialisms, pleasantries, marketing language, and exaggeration.\n\
         - Avoid addressing the reader directly: do not write “I”, “you”, “boss”, or “we”; for a Chinese target language, do not write “我”, “你”, “老板”, or “咱们”. Prefer neutral phrasing such as “This synthesis finds”, “Prioritize”, “The risk is”, and “The next step should be” (in Chinese: “本次综合认为”, “建议优先”, “风险在于”, and “下一步应”).\n\
         - Be restrained, clear, and actionable; do not fabricate certainty.\n\n",
    );
    s.push_str(&format!("Goal: {goal}\n\nWorker outputs:\n"));
    for (name, out) in workers {
        s.push_str(&format!("### {name}\n{out}\n\n"));
    }
    s
}

/// Parse lead agent stdout line by line and extract assistant text (Claude prefers final_text; Codex concatenates TextDelta).
pub(crate) fn collect_assistant_text(stdout: &[u8], parse: ParseFn) -> String {
    let parser: fn(&str) -> Vec<agent_event::AgentEvent> = match parse {
        ParseFn::Claude => agent_event::parse_claude_line,
        ParseFn::Codex => agent_event::parse_codex_line,
        ParseFn::Harness => agent_event::parse_harness_line,
        ParseFn::HarnessPlan => agent_event::parse_harness_plan_line,
    };
    let mut deltas: Vec<String> = Vec::new();
    let mut final_text: Option<String> = None;
    for line in String::from_utf8_lossy(stdout).lines() {
        for ev in parser(line) {
            match ev {
                agent_event::AgentEvent::Completed {
                    final_text: Some(t),
                    ..
                } => final_text = Some(t),
                agent_event::AgentEvent::TextDelta { text } => deltas.push(text),
                _ => {}
            }
        }
    }
    // 1. Each Codex agent_message is a complete message separated by \n (Opus NIT: prevents joined lines); Claude deltas are token-level and concatenated verbatim.
    // 2. Prefer final_text only when non-empty; otherwise use the delta buffer (Codex P1: prevents an empty/truncated Claude result from overwriting collected deltas).
    let sep = match parse {
        ParseFn::Codex => "\n",
        ParseFn::Claude | ParseFn::Harness | ParseFn::HarnessPlan => "",
    };
    let buf = deltas.join(sep);
    match final_text {
        Some(t) if !t.trim().is_empty() => t,
        _ => buf,
    }
}

pub(crate) fn make_backend(
    profile: &db::AgentProfile,
    key: Option<String>,
    search: HarnessSearchCreds,
    locale: Locale,
) -> Result<Box<dyn AgentBackend>, String> {
    match profile.access.as_str() {
        "native" => Ok(Box::new(NativeBackend {
            provider: profile.provider.clone(),
            primary_model: profile.primary_model.clone(),
        })),
        "borrow" => {
            let api_key = key.ok_or_else(|| ui_msg::al_err("agent.missingApiKey", &[]))?;
            Ok(Box::new(BorrowClaudeBackend {
                profile: profile.clone(),
                api_key,
            }))
        }
        "harness" => {
            validate_harness_agent_key(profile, key.as_deref(), locale)?;
            Ok(Box::new(HarnessBackend {
                profile: profile.clone(),
                api_key: key,
                search_api_key: search.key,
                search_backend: search.backend,
            }))
        }
        other => Err(ui_msg::al_err(
            "agent.unknownAccess",
            &[("access", other.to_string())],
        )),
    }
}

pub(crate) fn validate_harness_agent_key(
    profile: &db::AgentProfile,
    key: Option<&str>,
    locale: Locale,
) -> Result<(), String> {
    if profile.has_key && key.is_none() {
        let detail = match locale {
            Locale::Zh => "无法从系统钥匙串读取 API key。请打开 Settings，重新保存该 agent 的 API key。",
            Locale::En => "The API key could not be read from the system keychain. Open Settings and save this agent's API key again.",
        };
        return Err(ui_msg::al_err(
            "agent.keychainKeyUnavailable",
            &[("detail", detail.to_string())],
        ));
    }
    Ok(())
}

/// Pre-resolved harness search credentials, passed to `make_backend` after the caller completes keychain IPC outside the lock.
/// `backend` is the active search backend name (with the same "brave" fallback as before); `key` is that backend's configured API key (None when unset).
#[derive(Debug, Default, Clone)]
pub(crate) struct HarnessSearchCreds {
    pub(crate) key: Option<String>,
    pub(crate) backend: Option<String>,
}

/// Read the active search backend name (a DB read safe to call while holding the lock; the "brave" fallback matches the original `resolve_harness_search`).
pub(crate) fn active_search_backend_name(conn: &Connection) -> String {
    db::get_active_search_backend(conn).unwrap_or_else(|_| "brave".to_string())
}

/// Fetch the search key by backend name (real keychain IPC; the caller must invoke this outside the lock; trim/filter handling of empty strings matches the original function).
pub(crate) fn resolve_search_key(store: &dyn KeyStore, backend: &str) -> Option<String> {
    crate::keychain::get_search_key_with_store(store, backend)
        .ok()
        .flatten()
        .filter(|k| !k.trim().is_empty())
}

pub(crate) fn resolve_harness_search(
    conn: &Connection,
    store: &dyn KeyStore,
) -> (Option<String>, Option<String>) {
    let active = active_search_backend_name(conn);
    let key = resolve_search_key(store, &active);
    (key, Some(active))
}

/// Entry point for resolving harness profile search credentials outside the lock (N-2 narrowed item). Non-harness profiles return the default directly
/// without any DB read or keychain IPC (the `profile.access` check must happen first—this enforces the invariant that non-harness profiles must not
/// add any keychain IPC). For harness profiles, hold the lock briefly only while reading the backend name
/// (`db.0.lock()`, released immediately after the read), and perform the keychain IPC for the key strictly outside the lock.
/// Hard precondition for callers: the `db.0` lock must not already be held when calling this function; `TimedMutex` wraps a non-reentrant
/// `std::sync::Mutex`, so re-entering `lock()` on the same thread deadlocks immediately.
pub(crate) fn resolve_harness_search_creds(
    db: &Db,
    profile: &db::AgentProfile,
    store: &dyn KeyStore,
) -> Result<HarnessSearchCreds, String> {
    if profile.access != "harness" {
        return Ok(HarnessSearchCreds::default());
    }
    let backend = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        active_search_backend_name(&conn)
    };
    let key = resolve_search_key(store, &backend);
    Ok(HarnessSearchCreds {
        key,
        backend: Some(backend),
    })
}
