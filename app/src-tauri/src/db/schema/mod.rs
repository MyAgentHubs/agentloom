mod agent_migrations;
mod checkpoint_migrations;
mod core_tables;
mod decision_ledger_migration;
mod generated_reports;
mod memory_block_migrations;
mod message_agent_migrations;
mod message_migrations;
mod project_migrations;
mod remote_devices;
mod remote_inbox;
mod remote_tables;
mod repo_namespace_migrations;
mod session_group_migrations;
mod session_migrations;
mod session_runtime;

use rusqlite::Connection;

pub(super) fn apply_all(conn: &Connection) -> rusqlite::Result<()> {
    core_tables::apply(conn)?;
    generated_reports::apply(conn)?;
    checkpoint_migrations::apply(conn)?;
    message_agent_migrations::apply(conn)?;
    memory_block_migrations::apply(conn)?;
    agent_migrations::apply(conn)?;
    project_migrations::apply(conn)?;
    repo_namespace_migrations::apply(conn)?;
    session_migrations::apply(conn)?;
    session_group_migrations::apply(conn)?;
    decision_ledger_migration::apply(conn)?;
    message_migrations::apply(conn)?;
    session_runtime::apply(conn)?;
    remote_inbox::apply(conn)?;
    remote_devices::apply(conn)?;
    remote_tables::apply(conn)?;
    super::search_index::migrate(conn)?;
    Ok(())
}
