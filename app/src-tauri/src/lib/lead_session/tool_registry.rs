use super::super::*;

pub(crate) fn build_lead_tool_registry(
    app: &AppHandle,
    session_id: &str,
    run_id: &str,
    wt: &std::path::Path,
    lead_agent_id: &str,
    lead_agent_name: &str,
    lead_ctx: &std::sync::Arc<lead_tools::LeadCtx>,
) -> std::sync::Arc<mcp_server::ToolRegistry> {
    let mut tools = mcp_server::ToolRegistry::new();
    insert_dispatch_and_finish(&mut tools, lead_ctx);
    insert_memory_tools(&mut tools, app, session_id);
    insert_ask_user(&mut tools, app, session_id, lead_agent_id, lead_agent_name);
    insert_propose_verifier(&mut tools, app, session_id, lead_agent_id, lead_agent_name);
    insert_delivery_tools(&mut tools, app, session_id, run_id, wt);

    debug_assert_eq!(
        tools
            .keys()
            .map(String::as_str)
            .collect::<std::collections::HashSet<_>>(),
        LEAD_MCP_TOOL_NAMES.iter().copied().collect()
    );
    std::sync::Arc::new(tools)
}

fn insert_dispatch_and_finish(
    tools: &mut mcp_server::ToolRegistry,
    lead_ctx: &std::sync::Arc<lead_tools::LeadCtx>,
) {
    {
        let ctx = lead_ctx.clone();
        tools.insert(
            "dispatch_worker".to_string(),
            mcp_server::ToolDef {
                name: "dispatch_worker".to_string(),
                // Include the enabled member roster in the description so the lead can choose a valid agent on its first dispatch.
                // Let the lead see who is in the pool before making an invalid dispatch (an unmatched agent_hint).
                description: lead_tools::dispatch_worker_description(&ctx.member_pool),
                // Require agent_hint for multiple available agents and constrain it to the pool so dispatch selection is unambiguous.
                // Enum of valid agent_ids in the current pool; keep it optional when pool == 1.
                input_schema: lead_tools::dispatch_worker_input_schema(&ctx.member_pool),
                handler: Box::new(move |args: serde_json::Value| {
                    let task = args
                        .get("task")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    // Reject non-string agent_hint values so malformed selections cannot silently become unspecified dispatches.
                    let agent_hint = lead_tools::parse_agent_hint_arg(&args)?;
                    let goal_title = parse_goal_title_arg(&args);
                    lead_tools::dispatch_worker(
                        &ctx,
                        lead_tools::DispatchArgs {
                            task,
                            agent_hint,
                            goal_title,
                        },
                    )
                }),
            },
        );
    }
    {
        let ctx = lead_ctx.clone();
        tools.insert(
            "finish".to_string(),
            mcp_server::ToolDef {
                name: "finish".to_string(),
                description: LEAD_FINISH_DESCRIPTION.to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "evidence_refs": {"type": "array", "items": {"type": "string"}},
                        "rationale": {"type": "string"}
                    }
                }),
                handler: Box::new(move |args: serde_json::Value| {
                    let evidence_refs =
                        args.get("evidence_refs")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|e| e.as_str().map(|s| s.to_string()))
                                    .collect()
                            });
                    let rationale = args
                        .get("rationale")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    lead_tools::finish(
                        &ctx,
                        lead_tools::FinishArgs {
                            evidence_refs,
                            rationale,
                        },
                    )
                }),
            },
        );
    }
}

fn insert_memory_tools(tools: &mut mcp_server::ToolRegistry, app: &AppHandle, session_id: &str) {
    // Memory MCP tools (1d): the lead writes/reads the medical record through tools. The session is implicit (bound to this session).
    // Worker write permission remains for phase 3 (see lead_claude_argv_extra allowedTools: phase 1 only enables memory_* for the lead).
    {
        let app_m = app.clone();
        let sess_m = session_id.to_string();
        tools.insert(
            "memory_set".to_string(),
            mcp_server::ToolDef {
                name: "memory_set".to_string(),
                description: LEAD_MEMORY_SET_DESCRIPTION.to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "slot": {"type": "string", "enum": ["goal", "state", "next"]},
                        "text": {"type": "string"},
                        "title": {"type": "string"}
                    },
                    "required": ["slot", "text"]
                }),
                handler: Box::new(move |args: serde_json::Value| {
                    let db_state = app_m.state::<Db>();
                    let conn = db_state.0.lock().map_err(|e| e.to_string())?;
                    memory_tools::memory_set_tool(&conn, &sess_m, &args)
                }),
            },
        );
    }
    {
        let app_m = app.clone();
        let sess_m = session_id.to_string();
        tools.insert(
            "memory_add".to_string(),
            mcp_server::ToolDef {
                name: "memory_add".to_string(),
                description: LEAD_MEMORY_ADD_DESCRIPTION.to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "category": {"type": "string", "enum": ["decision", "pitfall", "risk", "watch"]},
                        "text": {"type": "string"},
                        "anchors": {"type": "array"},
                        "supersedes": {"type": "array", "items": {"type": "integer"}},
                        "confidence": {"type": "string"}
                    },
                    "required": ["category", "text"]
                }),
                handler: Box::new(move |args: serde_json::Value| {
                    let db_state = app_m.state::<Db>();
                    let conn = db_state.0.lock().map_err(|e| e.to_string())?;
                    memory_tools::memory_add_tool(&conn, &sess_m, &args)
                }),
            },
        );
    }
    {
        let app_m = app.clone();
        tools.insert(
            "memory_read_source".to_string(),
            mcp_server::ToolDef {
                name: "memory_read_source".to_string(),
                description: LEAD_MEMORY_READ_SOURCE_DESCRIPTION.to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "anchor": {"type": ["object", "array"]}
                    },
                    "required": ["anchor"]
                }),
                handler: Box::new(move |args: serde_json::Value| {
                    let db_state = app_m.state::<Db>();
                    let conn = db_state.0.lock().map_err(|e| e.to_string())?;
                    memory_tools::memory_read_source_tool(&conn, &args)
                }),
            },
        );
    }
}

fn insert_ask_user(
    tools: &mut mcp_server::ToolRegistry,
    app: &AppHandle,
    session_id: &str,
    lead_agent_id: &str,
    lead_agent_name: &str,
) {
    {
        let app_a = app.clone();
        let sess_a = session_id.to_string();
        // Clone the lead identity before ownership moves to the background thread so decision cards retain correct attribution.
        // See the profile_t = creds.profile move point below; clone a copy into the closure for decision cards/echo messages.
        let agent_id_a = lead_agent_id.to_string();
        let agent_name_a = lead_agent_name.to_string();
        tools.insert(
            "ask_user".to_string(),
            mcp_server::ToolDef {
                name: "ask_user".to_string(),
                description: LEAD_ASK_USER_DESCRIPTION.to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "question": {"type": "string"},
                        "options": {"type": "array", "items": {"type": "string"}},
                        "recommended": {"type": "string"},
                        "rationale": {"type": "string"}
                    },
                    "required": ["question", "options"]
                }),
                handler: Box::new(move |args: serde_json::Value| {
                    let question = args
                        .get("question")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let options = args
                        .get("options")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|e| e.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();
                    let recommended = args
                        .get("recommended")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let rationale = args
                        .get("rationale")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    lead_tools::ask_user_bounded(
                        &app_a,
                        &sess_a,
                        lead_tools::AskUserArgs {
                            question,
                            options,
                            recommended,
                            rationale,
                        },
                        Some(agent_id_a.as_str()),
                        Some(agent_name_a.as_str()),
                    )
                }),
            },
        );
    }
}

fn insert_propose_verifier(
    tools: &mut mcp_server::ToolRegistry,
    app: &AppHandle,
    session_id: &str,
    lead_agent_id: &str,
    lead_agent_name: &str,
) {
    {
        let app_pv = app.clone();
        let sess_pv = session_id.to_string();
        // Preserve the lead identity on verifier confirmation and automatic-run result cards for consistent attribution.
        let agent_id_pv = lead_agent_id.to_string();
        let agent_name_pv = lead_agent_name.to_string();
        tools.insert(
            "propose_verifier".to_string(),
            mcp_server::ToolDef {
                name: "propose_verifier".to_string(),
                description: LEAD_PROPOSE_VERIFIER_DESCRIPTION.to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "cmd": {"type": "string"},
                        "rationale": {"type": "string"}
                    },
                    "required": ["cmd"]
                }),
                handler: Box::new(move |args: serde_json::Value| {
                    let cmd = args
                        .get("cmd")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let rationale = args
                        .get("rationale")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    lead_tools::propose_verifier(
                        &app_pv,
                        &sess_pv,
                        lead_tools::ProposeVerifierArgs { cmd, rationale },
                        Some(agent_id_pv.as_str()),
                        Some(agent_name_pv.as_str()),
                    )
                }),
            },
        );
    }
}

fn insert_delivery_tools(
    tools: &mut mcp_server::ToolRegistry,
    app: &AppHandle,
    session_id: &str,
    run_id: &str,
    wt: &std::path::Path,
) {
    {
        tools.insert(
            "commit".to_string(),
            build_commit_tool(app, session_id, run_id, wt),
        );
        tools.insert("push".to_string(), build_push_tool(app, session_id, run_id));
        tools.insert(
            "create_pr".to_string(),
            build_create_pr_tool(app, session_id, run_id),
        );
        tools.insert(
            "publish".to_string(),
            build_publish_tool(app, session_id, run_id),
        );
    }
}
