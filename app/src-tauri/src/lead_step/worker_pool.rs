use crate::lead_action::{LeadAction, LeadActionParseError};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WorkerPoolEntry {
    pub id: String,
    pub name: String,
    pub provider: String,
}

/// Render the worker pool as a one-line list for the lead to choose from (by id/name/provider, used in retry_hint).
fn pool_summary(pool: &[WorkerPoolEntry], locale: crate::Locale) -> String {
    pool.iter()
        .map(|w| format!("id={}/name={}/provider={}", w.id, w.name, w.provider))
        .collect::<Vec<_>>()
        .join(match locale {
            crate::Locale::Zh => "；",
            crate::Locale::En => "; ",
        })
}

/// Workers matched by agent_hint (case-insensitive exact equality, with no fallback).
fn pool_hint_matches<'a>(pool: &'a [WorkerPoolEntry], hint: &str) -> Vec<&'a WorkerPoolEntry> {
    let h = hint.trim().to_lowercase();
    pool.iter()
        .filter(|w| {
            w.id.to_lowercase() == h || w.name.to_lowercase() == h || w.provider.to_lowercase() == h
        })
        .collect()
}

/// Validate whether a dispatch_worker action is legal for the current [Dispatchable workers] pool
/// (inside the lead retry loop; an invalid action triggers a retry and never silently falls back).
/// - An empty pool is invalid because there is nobody to dispatch.
/// - With agent_hint, it must case-insensitively and exactly match the id/name/provider of exactly one worker in the pool; otherwise it is invalid.
/// - When the pool has more than one worker, omitting agent_hint is invalid because one worker must be specified.
/// - When the pool has exactly one worker, omitting agent_hint is valid because there is no ambiguity.
/// Actions other than dispatch_worker are not constrained by the pool.
pub fn validate_dispatch_against_pool(
    action: &LeadAction,
    pool: &[WorkerPoolEntry],
    locale: crate::Locale,
) -> Result<(), LeadActionParseError> {
    let LeadAction::DispatchWorker { agent_hint, .. } = action else {
        return Ok(());
    };
    if pool.is_empty() {
        return Err(LeadActionParseError::SemanticInvalid(match locale {
            crate::Locale::Zh => "当前没有可调度的 worker（【可调度 worker】池为空）·不能 dispatch_worker·改用 reply 或 ask_user".into(),
            crate::Locale::En => "No dispatchable workers are available (the [Dispatchable workers] pool is empty); cannot dispatch_worker; use reply or ask_user instead".into(),
        }));
    }
    match agent_hint
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty())
    {
        Some(hint) => {
            let matches = pool_hint_matches(pool, hint);
            match matches.len() {
                0 => Err(LeadActionParseError::SemanticInvalid(match locale {
                    crate::Locale::Zh => format!(
                        "agent_hint「{hint}」不在【可调度 worker】池里·请从这些里选一个 id/name/provider：{}",
                        pool_summary(pool, locale)
                    ),
                    crate::Locale::En => format!(
                        "agent_hint \"{hint}\" is not in the [Dispatchable workers] pool; pick one of these id/name/provider: {}",
                        pool_summary(pool, locale)
                    ),
                })),
                1 => Ok(()),
                _ => Err(LeadActionParseError::SemanticInvalid(match locale {
                    crate::Locale::Zh => format!(
                        "agent_hint「{hint}」命中多个【可调度 worker】·请改用唯一 id/name/provider：{}",
                        pool_summary(pool, locale)
                    ),
                    crate::Locale::En => format!(
                        "agent_hint \"{hint}\" matches multiple workers in [Dispatchable workers]; use a unique id/name/provider: {}",
                        pool_summary(pool, locale)
                    ),
                })),
            }
        }
        None => {
            if pool.len() == 1 {
                Ok(())
            } else {
                Err(LeadActionParseError::SemanticInvalid(match locale {
                    crate::Locale::Zh => format!(
                        "【可调度 worker】超过 1 个·dispatch_worker 必须带 agent_hint 指定一个 worker（可选：{}）",
                        pool_summary(pool, locale)
                    ),
                    crate::Locale::En => format!(
                        "[Dispatchable workers] has more than one worker; dispatch_worker must include agent_hint to select one (options: {})",
                        pool_summary(pool, locale)
                    ),
                }))
            }
        }
    }
}

pub fn build_worker_pool(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<WorkerPoolEntry>, String> {
    let config = match crate::db::get_session_agent_config(conn, session_id) {
        Ok(config) => config,
        Err(err) if err.to_string().contains("does not exist") => return Ok(Vec::new()),
        Err(err) => return Err(err.to_string()),
    };
    let Some(lead_id) = config.lead_agent_id.as_deref() else {
        return Ok(Vec::new());
    };
    let wanted: HashSet<&str> = config.member_agent_ids.iter().map(String::as_str).collect();
    if wanted.is_empty() {
        return Ok(Vec::new());
    }

    let agents = crate::db::list_agents(conn).map_err(|e| e.to_string())?;
    let by_id: HashMap<String, crate::db::AgentProfile> = agents
        .into_iter()
        .map(|agent| (agent.id.clone(), agent))
        .collect();
    let mut out = Vec::new();
    for member_id in &config.member_agent_ids {
        if member_id == lead_id || !wanted.contains(member_id.as_str()) {
            continue;
        }
        let Some(agent) = by_id.get(member_id) else {
            continue;
        };
        if !agent.enabled {
            continue;
        }
        out.push(WorkerPoolEntry {
            id: agent.id.clone(),
            name: agent.name.clone(),
            provider: agent.provider.clone(),
        });
    }
    Ok(out)
}

/// Render the [Dispatchable workers] pool for the lead:
/// - `dispatchable_member_ids = None` falls back to `build_worker_pool` for backward compatibility and tests.
/// - `Some(ids)` intersects the saved session member configuration with the ids the frontend can actually dispatch this turn:
///   retain only workers that are in the saved member configuration, enabled, and not the lead itself; emit them in the order supplied by the frontend.
///   `Some([])` produces an empty pool. Any id absent from the saved configuration, disabled, or belonging to the lead is ignored.
pub fn build_worker_pool_with_override(
    conn: &Connection,
    session_id: &str,
    dispatchable_member_ids: Option<&[String]>,
) -> Result<Vec<WorkerPoolEntry>, String> {
    let Some(ids) = dispatchable_member_ids else {
        return build_worker_pool(conn, session_id);
    };
    let config = match crate::db::get_session_agent_config(conn, session_id) {
        Ok(config) => config,
        Err(err) if err.to_string().contains("does not exist") => return Ok(Vec::new()),
        Err(err) => return Err(err.to_string()),
    };
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let Some(lead_id) = config.lead_agent_id.as_deref() else {
        return Ok(Vec::new());
    };
    let saved: HashSet<&str> = config.member_agent_ids.iter().map(String::as_str).collect();

    let agents = crate::db::list_agents(conn).map_err(|e| e.to_string())?;
    let by_id: HashMap<String, crate::db::AgentProfile> = agents
        .into_iter()
        .map(|agent| (agent.id.clone(), agent))
        .collect();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out = Vec::new();
    for member_id in ids {
        if !seen.insert(member_id.as_str()) {
            continue; // dedupe: the frontend may send duplicate ids
        }
        if member_id == lead_id || !saved.contains(member_id.as_str()) {
            continue;
        }
        let Some(agent) = by_id.get(member_id) else {
            continue;
        };
        if !agent.enabled {
            continue;
        }
        out.push(WorkerPoolEntry {
            id: agent.id.clone(),
            name: agent.name.clone(),
            provider: agent.provider.clone(),
        });
    }
    Ok(out)
}
