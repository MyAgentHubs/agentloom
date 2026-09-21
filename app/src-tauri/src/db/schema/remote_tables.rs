use rusqlite::Connection;

pub(super) fn apply(conn: &Connection) -> rusqlite::Result<()> {
    // The desktop-authoritative generation source is monotonic within each room.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS remote_registry_counter (
            room_id TEXT PRIMARY KEY,
            next_generation INTEGER NOT NULL CHECK (next_generation > 0)
        )",
        [],
    )?;
    // Store the per-project room mapping alongside the legacy global remote_room_id setting.
    // This creates new schema rather than migrating or deleting the legacy setting; the gateway
    // continues reading the old value until its later migration. Room IDs retain the existing
    // 128-bit CSPRNG representation as 32 lowercase hexadecimal characters.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS project_remote_rooms (
            project_id TEXT PRIMARY KEY,
            room_id TEXT NOT NULL UNIQUE,
            created_at_ms INTEGER NOT NULL
        )",
        [],
    )?;
    Ok(())
}
