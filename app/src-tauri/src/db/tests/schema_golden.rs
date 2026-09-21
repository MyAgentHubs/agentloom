#![cfg(test)]

use super::*;

fn schema_dump(conn: &Connection) -> rusqlite::Result<String> {
    let mut output = String::new();
    let mut stmt = conn.prepare(
        "SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY type, name, rowid",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;
    for row in rows {
        let (object_type, name, table_name, sql) = row?;
        let sql = sql
            .unwrap_or_default()
            .replace('\\', "\\\\")
            .replace('|', "\\|")
            .replace('\r', "\\r")
            .replace('\n', "\\n");
        output.push_str(&format!("{object_type}|{name}|{table_name}|{sql}\n"));
    }
    let user_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    output.push_str(&format!("PRAGMA user_version|{user_version}\n"));
    Ok(output)
}

#[test]
fn init_schema_matches_golden_dump() {
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();
    let actual = schema_dump(&conn).unwrap();
    assert_eq!(actual, include_str!("fixtures/schema_golden.sql"));
}
