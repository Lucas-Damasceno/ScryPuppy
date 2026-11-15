//! Atomic context assignments shared by desktop frontends.
//! Callers must refresh search indexes and encrypted context exports after commit.
use crate::app_error::{AppError, CommandResult};
use rusqlite::{params, Connection, OptionalExtension};

pub fn validate_capture_exists(conn: &Connection, capture_id: &str) -> CommandResult<()> {
    let exists = conn
        .query_row("SELECT 1 FROM captures WHERE id = ?", [capture_id], |_| {
            Ok(())
        })
        .optional()
        .map_err(|e| e.to_string())?;
    exists.ok_or_else(|| AppError::new("capture.not_found"))
}

pub fn validate_user_context_exists(conn: &Connection, context_id: &str) -> CommandResult<()> {
    if context_id == "inbox" || context_id == "knowledge-base" {
        return Err(AppError::new("context.reserved_collection"));
    }
    let exists = conn
        .query_row("SELECT 1 FROM contexts WHERE id = ?", [context_id], |_| {
            Ok(())
        })
        .optional()
        .map_err(|e| e.to_string())?;
    exists.ok_or_else(|| AppError::new("context.not_found"))
}

pub fn add_capture_contexts<F: FnMut() -> String>(
    conn: &mut Connection,
    capture_id: &str,
    context_ids: &[String],
    mut now: F,
) -> CommandResult<()> {
    let transaction = conn.transaction().map_err(|e| e.to_string())?;
    validate_capture_exists(&transaction, capture_id)?;
    for context_id in context_ids {
        validate_user_context_exists(&transaction, context_id)?;
        transaction.execute(
            "INSERT OR IGNORE INTO capture_contexts (capture_id, context_id, assignment_origin, confidence, created_at) VALUES (?, ?, 'manual', NULL, ?)",
            params![capture_id, context_id, now()],
        ).map_err(|e| e.to_string())?;
    }
    transaction.commit().map_err(|e| e.to_string())?;
    Ok(())
}

pub fn add_captures_to_context(
    conn: &mut Connection,
    capture_ids: &[String],
    context_id: &str,
    assigned_at: &str,
) -> CommandResult<usize> {
    if capture_ids.is_empty() {
        return Ok(0);
    }
    let transaction = conn.transaction().map_err(|e| e.to_string())?;
    validate_user_context_exists(&transaction, context_id)?;
    let mut added_count = 0;
    for capture_id in capture_ids {
        validate_capture_exists(&transaction, capture_id)?;
        added_count += transaction.execute(
            "INSERT OR IGNORE INTO capture_contexts (capture_id, context_id, assignment_origin, confidence, created_at) VALUES (?, ?, 'manual', NULL, ?)",
            params![capture_id, context_id, assigned_at],
        ).map_err(|e| e.to_string())?;
    }
    transaction.commit().map_err(|e| e.to_string())?;
    Ok(added_count)
}

pub fn remove_capture_context(
    conn: &Connection,
    capture_id: &str,
    context_id: &str,
) -> CommandResult<()> {
    validate_capture_exists(conn, capture_id)?;
    conn.execute(
        "DELETE FROM capture_contexts WHERE capture_id = ? AND context_id = ?",
        params![capture_id, context_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn database() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE captures(id TEXT PRIMARY KEY); CREATE TABLE contexts(id TEXT PRIMARY KEY);\
            CREATE TABLE capture_contexts(capture_id TEXT, context_id TEXT, assignment_origin TEXT, confidence REAL, created_at TEXT, PRIMARY KEY(capture_id, context_id));\
            INSERT INTO captures VALUES ('c1'), ('c2'); INSERT INTO contexts VALUES ('work'), ('personal');").unwrap();
        conn
    }
    #[test]
    fn multi_assignment_is_atomic_and_idempotent() {
        let mut conn = database();
        let invalid = vec!["work".to_string(), "missing".to_string()];
        assert_eq!(
            add_capture_contexts(&mut conn, "c1", &invalid, || "time".into())
                .unwrap_err()
                .code,
            "context.not_found"
        );
        assert_eq!(
            conn.query_row("SELECT count(*) FROM capture_contexts", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let valid = vec!["work".to_string(), "personal".to_string()];
        add_capture_contexts(&mut conn, "c1", &valid, || "time".into()).unwrap();
        add_capture_contexts(&mut conn, "c1", &valid, || "time".into()).unwrap();
        assert_eq!(
            conn.query_row("SELECT count(*) FROM capture_contexts", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
    }
    #[test]
    fn bulk_assignment_rolls_back_and_reserved_contexts_are_rejected() {
        let mut conn = database();
        assert_eq!(
            add_captures_to_context(&mut conn, &["c1".into(), "missing".into()], "work", "time")
                .unwrap_err()
                .code,
            "capture.not_found"
        );
        assert_eq!(
            conn.query_row("SELECT count(*) FROM capture_contexts", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            add_captures_to_context(&mut conn, &["c1".into()], "inbox", "time")
                .unwrap_err()
                .code,
            "context.reserved_collection"
        );
        assert_eq!(
            add_captures_to_context(&mut conn, &["c1".into(), "c2".into()], "work", "time")
                .unwrap(),
            2
        );
        remove_capture_context(&conn, "c1", "work").unwrap();
        assert_eq!(
            conn.query_row("SELECT count(*) FROM capture_contexts", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}
