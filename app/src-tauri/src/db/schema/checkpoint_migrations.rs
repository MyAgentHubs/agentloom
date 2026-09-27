use rusqlite::Connection;

pub(super) fn apply(conn: &Connection) -> rusqlite::Result<()> {
    // Checkpoint path/preimage/undo columns (idempotent legacy migration). The post_* columns are
    // retained only for database compatibility; undo no longer reads or writes them.
    let checkpoint_entry_cols = {
        let mut stmt = conn.prepare("PRAGMA table_info(checkpoint_entries)")?;
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        columns
    };
    for (column, declaration) in [
        ("allowed_root", "TEXT"),
        ("pre_xattrs", "BLOB"),
        ("post_sha", "TEXT"),
        ("post_missing", "INTEGER NOT NULL DEFAULT 0"),
        ("post_file_type", "TEXT"),
        ("post_mode", "INTEGER"),
        ("post_nlink", "INTEGER"),
        ("post_inode", "INTEGER"),
        ("post_xattr_sha", "TEXT"),
        ("post_tainted", "INTEGER NOT NULL DEFAULT 0"),
        ("undone_at", "INTEGER"),
    ] {
        if !checkpoint_entry_cols
            .iter()
            .any(|existing| existing == column)
        {
            conn.execute(
                &format!("ALTER TABLE checkpoint_entries ADD COLUMN {column} {declaration}"),
                [],
            )?;
        }
    }
    Ok(())
}
