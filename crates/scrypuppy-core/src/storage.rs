//! Shared SQLCipher connection setup. Existing data must never be opened without
//! its key or silently replaced when a key is missing or wrong.
use rusqlite::{Connection, OpenFlags};
use std::{fs, path::Path};

pub const DATABASE_FILE: &str = "scryppy.sqlite";

pub fn apply_database_key(conn: &Connection, key: &str) -> Result<(), String> {
    if key.len() != 64 || !key.chars().all(|character| character.is_ascii_hexdigit()) {
        return Err("Chave local do banco inválida.".into());
    }
    conn.execute_batch(&format!("PRAGMA key = \"x'{key}'\";"))
        .map_err(|e| e.to_string())
}

fn configure(conn: Connection, key: &str) -> Result<Connection, String> {
    apply_database_key(&conn, key)?;
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;")
        .map_err(|e| e.to_string())?;
    Ok(conn)
}

/// Retains the existing shell's creation behavior, used after initialization.
pub fn open_or_create(app_dir: &Path, key: &str) -> Result<Connection, String> {
    fs::create_dir_all(app_dir).map_err(|e| e.to_string())?;
    let conn = Connection::open(app_dir.join(DATABASE_FILE)).map_err(|e| e.to_string())?;
    configure(conn, key)
}

/// Opens an existing encrypted database without creating or modifying it.
/// Verify the key immediately so callers cannot mistake a wrong key for an
/// empty library. The caller is responsible for locating the original data and key.
pub fn open_existing_read_only(path: &Path, key: &str) -> Result<Connection, String> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| e.to_string())?;
    let conn = configure(conn, key)?;
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
        r.get::<_, i64>(0)
    })
    .map_err(|e| format!("Não foi possível abrir a base criptografada: {e}"))?;
    Ok(conn)
}

/// Open an existing keyed database for edits without ever creating the file.
/// This is reserved for native actions that have their matching index/export
/// side effects available in the shared core.
pub fn open_existing_read_write(path: &Path, key: &str) -> Result<Connection, String> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|e| e.to_string())?;
    let conn = configure(conn, key)?;
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
        r.get::<_, i64>(0)
    })
    .map_err(|e| format!("Não foi possível abrir a base criptografada: {e}"))?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_database_requires_correct_key_and_cannot_be_modified() {
        let dir = tempfile::tempdir().unwrap();
        let key = "12".repeat(32);
        let conn = open_or_create(dir.path(), &key).unwrap();
        conn.execute_batch("CREATE TABLE captures(id TEXT); INSERT INTO captures VALUES ('one');")
            .unwrap();
        drop(conn);
        let path = dir.path().join(DATABASE_FILE);
        assert!(open_existing_read_only(&path, &"34".repeat(32)).is_err());
        let conn = open_existing_read_only(&path, &key).unwrap();
        assert_eq!(
            conn.query_row("SELECT count(*) FROM captures", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert!(conn.execute("DELETE FROM captures", []).is_err());
    }

    #[test]
    fn opening_missing_file_does_not_create_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DATABASE_FILE);
        assert!(open_existing_read_only(&path, &"12".repeat(32)).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn existing_database_can_be_opened_for_write_but_never_created() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DATABASE_FILE);
        let key = "56".repeat(32);
        assert!(open_existing_read_write(&path, &key).is_err());
        assert!(!path.exists());

        let conn = open_or_create(dir.path(), &key).unwrap();
        conn.execute_batch("CREATE TABLE records(value TEXT);")
            .unwrap();
        drop(conn);
        let conn = open_existing_read_write(&path, &key).unwrap();
        conn.execute("INSERT INTO records VALUES ('updated')", [])
            .unwrap();
        assert_eq!(
            conn.query_row("SELECT value FROM records", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "updated"
        );
        assert!(open_existing_read_write(&path, &"67".repeat(32)).is_err());
    }
}
