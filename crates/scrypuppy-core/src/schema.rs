//! The canonical database schema migration shared by desktop frontends.
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};

pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        PRAGMA foreign_keys = ON;

        CREATE TABLE IF NOT EXISTS contexts (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            normalized_name TEXT NOT NULL DEFAULT '',
            slug TEXT NOT NULL UNIQUE,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS captures (
            id TEXT PRIMARY KEY,
            context_id TEXT NOT NULL REFERENCES contexts(id) ON DELETE RESTRICT,
            content_text TEXT NOT NULL,
            content_hash TEXT NOT NULL,
            captured_at TEXT NOT NULL,
            source_app_name TEXT,
            source_app_id TEXT,
            source_process_id INTEGER,
            source_process_path TEXT,
            window_title TEXT,
            window_id TEXT,
            platform TEXT NOT NULL,
            metadata_json TEXT NOT NULL,
            capture_kind TEXT NOT NULL DEFAULT 'capture',
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS capture_assets (
            id TEXT PRIMARY KEY,
            capture_id TEXT NOT NULL REFERENCES captures(id) ON DELETE CASCADE,
            kind TEXT NOT NULL,
            path TEXT,
            status TEXT NOT NULL,
            error TEXT,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS capture_representations (
            id TEXT PRIMARY KEY,
            capture_id TEXT NOT NULL REFERENCES captures(id) ON DELETE CASCADE,
            ordinal INTEGER NOT NULL,
            kind TEXT NOT NULL,
            format_name TEXT NOT NULL,
            mime_type TEXT,
            text_content TEXT,
            asset_path TEXT,
            size_bytes INTEGER,
            sha256 TEXT,
            restorable INTEGER NOT NULL DEFAULT 1,
            metadata_json TEXT NOT NULL DEFAULT '{}',
            created_at TEXT NOT NULL,
            UNIQUE(capture_id, ordinal)
        );

        CREATE TABLE IF NOT EXISTS capture_file_entries (
            id TEXT PRIMARY KEY,
            capture_id TEXT NOT NULL REFERENCES captures(id) ON DELETE CASCADE,
            representation_id TEXT NOT NULL REFERENCES capture_representations(id) ON DELETE CASCADE,
            ordinal INTEGER NOT NULL,
            display_name TEXT NOT NULL,
            original_path TEXT,
            local_path TEXT,
            entry_kind TEXT NOT NULL,
            extension TEXT,
            size_bytes INTEGER,
            sha256 TEXT,
            availability TEXT NOT NULL,
            metadata_json TEXT NOT NULL DEFAULT '{}',
            created_at TEXT NOT NULL,
            UNIQUE(representation_id, ordinal)
        );

        CREATE TABLE IF NOT EXISTS capture_clipboard_formats (
            capture_id TEXT NOT NULL REFERENCES captures(id) ON DELETE CASCADE,
            format_id INTEGER NOT NULL,
            format_name TEXT NOT NULL,
            supported INTEGER NOT NULL,
            PRIMARY KEY(capture_id, format_id)
        );

        CREATE INDEX IF NOT EXISTS idx_capture_representations_capture
            ON capture_representations(capture_id, ordinal);
        CREATE INDEX IF NOT EXISTS idx_capture_file_entries_capture
            ON capture_file_entries(capture_id, ordinal);

        CREATE TABLE IF NOT EXISTS capture_tags (
            capture_id TEXT NOT NULL REFERENCES captures(id) ON DELETE CASCADE,
            tag TEXT NOT NULL,
            source TEXT NOT NULL,
            created_at TEXT NOT NULL,
            PRIMARY KEY (capture_id, tag)
        );

        CREATE TABLE IF NOT EXISTS capture_contexts (
            capture_id TEXT NOT NULL REFERENCES captures(id) ON DELETE CASCADE,
            context_id TEXT NOT NULL REFERENCES contexts(id) ON DELETE CASCADE,
            assignment_origin TEXT NOT NULL CHECK (assignment_origin IN ('manual', 'automatic')),
            confidence REAL,
            created_at TEXT NOT NULL,
            PRIMARY KEY (capture_id, context_id)
        );

        CREATE TABLE IF NOT EXISTS context_rules (
            id TEXT PRIMARY KEY,
            context_id TEXT NOT NULL REFERENCES contexts(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            enabled INTEGER NOT NULL DEFAULT 1,
            match_mode TEXT NOT NULL CHECK (match_mode IN ('all', 'any')),
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS context_rule_conditions (
            id TEXT PRIMARY KEY,
            rule_id TEXT NOT NULL REFERENCES context_rules(id) ON DELETE CASCADE,
            ordinal INTEGER NOT NULL,
            field TEXT NOT NULL,
            operator TEXT NOT NULL,
            value TEXT NOT NULL,
            created_at TEXT NOT NULL,
            UNIQUE(rule_id, ordinal)
        );

        CREATE TABLE IF NOT EXISTS capture_entities (
            id TEXT PRIMARY KEY,
            capture_id TEXT NOT NULL REFERENCES captures(id) ON DELETE CASCADE,
            kind TEXT NOT NULL,
            value TEXT NOT NULL,
            source TEXT NOT NULL,
            confidence REAL NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS capture_ocr (
            capture_id TEXT PRIMARY KEY REFERENCES captures(id) ON DELETE CASCADE,
            status TEXT NOT NULL,
            text TEXT,
            error TEXT,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS ocr_jobs (
            capture_id TEXT PRIMARY KEY REFERENCES captures(id) ON DELETE CASCADE,
            asset_id TEXT NOT NULL REFERENCES capture_assets(id) ON DELETE CASCADE,
            status TEXT NOT NULL,
            attempts INTEGER NOT NULL DEFAULT 0,
            queued_at TEXT NOT NULL,
            started_at TEXT,
            completed_at TEXT,
            error TEXT
        );

        CREATE TABLE IF NOT EXISTS settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS capture_search_documents (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            capture_id TEXT NOT NULL UNIQUE REFERENCES captures(id) ON DELETE CASCADE,
            content TEXT NOT NULL DEFAULT '',
            ocr TEXT NOT NULL DEFAULT '',
            metadata TEXT NOT NULL DEFAULT '',
            content_hash TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS capture_search_fts USING fts5(
            content,
            ocr,
            metadata,
            content='capture_search_documents',
            content_rowid='id',
            tokenize='unicode61 remove_diacritics 2'
        );

        CREATE TRIGGER IF NOT EXISTS capture_search_documents_ai AFTER INSERT ON capture_search_documents BEGIN
            INSERT INTO capture_search_fts(rowid, content, ocr, metadata)
            VALUES (new.id, new.content, new.ocr, new.metadata);
        END;
        CREATE TRIGGER IF NOT EXISTS capture_search_documents_ad AFTER DELETE ON capture_search_documents BEGIN
            INSERT INTO capture_search_fts(capture_search_fts, rowid, content, ocr, metadata)
            VALUES ('delete', old.id, old.content, old.ocr, old.metadata);
        END;
        CREATE TRIGGER IF NOT EXISTS capture_search_documents_au AFTER UPDATE ON capture_search_documents BEGIN
            INSERT INTO capture_search_fts(capture_search_fts, rowid, content, ocr, metadata)
            VALUES ('delete', old.id, old.content, old.ocr, old.metadata);
            INSERT INTO capture_search_fts(rowid, content, ocr, metadata)
            VALUES (new.id, new.content, new.ocr, new.metadata);
        END;

        CREATE TABLE IF NOT EXISTS capture_embeddings (
            capture_id TEXT PRIMARY KEY REFERENCES captures(id) ON DELETE CASCADE,
            model_id TEXT NOT NULL,
            index_version INTEGER NOT NULL,
            dimensions INTEGER NOT NULL,
            content_hash TEXT NOT NULL,
            vector BLOB NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS magic_search_documents (
            id TEXT PRIMARY KEY,
            root_id TEXT NOT NULL,
            previous_document_id TEXT,
            version INTEGER NOT NULL,
            title TEXT NOT NULL,
            query TEXT NOT NULL,
            markdown TEXT NOT NULL,
            provider TEXT NOT NULL,
            model TEXT NOT NULL,
            filters_json TEXT NOT NULL,
            generation_warning TEXT,
            evidence_count INTEGER NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS magic_search_evidence (
            document_id TEXT NOT NULL REFERENCES magic_search_documents(id) ON DELETE CASCADE,
            capture_id TEXT NOT NULL,
            rank INTEGER NOT NULL,
            score INTEGER NOT NULL,
            captured_at TEXT,
            context_name TEXT,
            app_name TEXT,
            application_id TEXT,
            window_title TEXT,
            excerpt TEXT,
            asset_paths_json TEXT NOT NULL DEFAULT '[]',
            PRIMARY KEY (document_id, capture_id)
        );

        CREATE INDEX IF NOT EXISTS idx_captures_captured_at ON captures(captured_at);
        CREATE INDEX IF NOT EXISTS idx_capture_contexts_context ON capture_contexts(context_id, created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_context_rules_context ON context_rules(context_id, enabled, updated_at DESC);
        CREATE INDEX IF NOT EXISTS idx_context_rule_conditions_rule ON context_rule_conditions(rule_id, ordinal);
        CREATE INDEX IF NOT EXISTS idx_capture_tags_tag ON capture_tags(tag);
        CREATE INDEX IF NOT EXISTS idx_capture_entities_kind_value ON capture_entities(kind, value);
        CREATE INDEX IF NOT EXISTS idx_ocr_jobs_status ON ocr_jobs(status, queued_at);
        CREATE INDEX IF NOT EXISTS idx_magic_search_root ON magic_search_documents(root_id, version DESC);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_magic_search_root_version ON magic_search_documents(root_id, version);
        CREATE INDEX IF NOT EXISTS idx_magic_search_evidence_document ON magic_search_evidence(document_id, rank);
        CREATE INDEX IF NOT EXISTS idx_capture_embeddings_model
            ON capture_embeddings(model_id, index_version, dimensions);
        ",
    )?;

    if !column_exists(conn, "contexts", "normalized_name")? {
        conn.execute(
            "ALTER TABLE contexts ADD COLUMN normalized_name TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }
    conn.execute(
        "UPDATE contexts SET normalized_name = lower(trim(name)) WHERE normalized_name = ''",
        [],
    )?;
    conn.execute("DROP INDEX IF EXISTS idx_contexts_normalized_name", [])?;
    conn.execute(
        "INSERT OR IGNORE INTO contexts (id, name, normalized_name, slug, created_at, updated_at)
         SELECT 'knowledge-base', 'Knowledge Base', 'knowledge base', 'knowledge-base', created_at, updated_at
         FROM contexts WHERE id = 'content-base'",
        [],
    )?;
    conn.execute(
        "UPDATE contexts SET name = 'Knowledge Base', normalized_name = 'knowledge base', slug = 'knowledge-base'
         WHERE id = 'knowledge-base'",
        [],
    )?;
    conn.execute(
        "UPDATE captures SET context_id = 'knowledge-base' WHERE context_id = 'content-base'",
        [],
    )?;
    conn.execute(
        "UPDATE OR IGNORE capture_contexts SET context_id = 'knowledge-base' WHERE context_id = 'content-base'",
        [],
    )?;
    conn.execute(
        "DELETE FROM capture_contexts WHERE context_id = 'content-base'",
        [],
    )?;
    conn.execute("DELETE FROM contexts WHERE id = 'content-base'", [])?;
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_contexts_normalized_name
         ON contexts(normalized_name) WHERE id NOT IN ('inbox', 'knowledge-base')",
        [],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO capture_contexts
            (capture_id, context_id, assignment_origin, confidence, created_at)
         SELECT id, context_id, 'manual', NULL, created_at FROM captures
         WHERE context_id NOT IN ('inbox', 'knowledge-base')",
        [],
    )?;
    conn.execute(
        "UPDATE captures SET context_id = CASE WHEN capture_kind = 'reference' THEN 'knowledge-base' ELSE 'inbox' END
         WHERE context_id NOT IN ('inbox', 'knowledge-base')",
        [],
    )?;
    conn.execute_batch(
        "DROP TABLE IF EXISTS capture_project_categories;
         DROP TABLE IF EXISTS project_category_anchors;
         DROP TABLE IF EXISTS project_categories;",
    )?;

    if !column_exists(conn, "captures", "capture_kind")? {
        conn.execute(
            "ALTER TABLE captures ADD COLUMN capture_kind TEXT NOT NULL DEFAULT 'capture'",
            [],
        )?;
    }

    if !column_exists(conn, "magic_search_documents", "generation_warning")? {
        conn.execute(
            "ALTER TABLE magic_search_documents ADD COLUMN generation_warning TEXT",
            [],
        )?;
    }
    if !column_exists(conn, "magic_search_documents", "retrieval_engine")? {
        conn.execute(
            "ALTER TABLE magic_search_documents ADD COLUMN retrieval_engine TEXT NOT NULL DEFAULT 'legacy'",
            [],
        )?;
    }
    if !column_exists(conn, "magic_search_documents", "retrieval_model")? {
        conn.execute(
            "ALTER TABLE magic_search_documents ADD COLUMN retrieval_model TEXT",
            [],
        )?;
    }
    for (column, definition) in [
        ("captured_at", "TEXT"),
        ("context_name", "TEXT"),
        ("app_name", "TEXT"),
        ("application_id", "TEXT"),
        ("window_title", "TEXT"),
        ("excerpt", "TEXT"),
        ("asset_paths_json", "TEXT NOT NULL DEFAULT '[]'"),
    ] {
        if !column_exists(conn, "magic_search_evidence", column)? {
            conn.execute(
                &format!("ALTER TABLE magic_search_evidence ADD COLUMN {column} {definition}"),
                [],
            )?;
        }
    }
    conn.execute_batch(
        "UPDATE magic_search_evidence
         SET captured_at = COALESCE(captured_at, (
                 SELECT c.captured_at FROM captures c WHERE c.id = magic_search_evidence.capture_id
             )),
             context_name = COALESCE(context_name, (
                 SELECT group_concat(co.name, ', ') FROM capture_contexts cc
                 JOIN contexts co ON co.id = cc.context_id
                 WHERE cc.capture_id = magic_search_evidence.capture_id
             ), ''),
             app_name = COALESCE(app_name, (
                 SELECT c.source_app_name FROM captures c WHERE c.id = magic_search_evidence.capture_id
             )),
             application_id = COALESCE(application_id, (
                 SELECT c.source_app_id FROM captures c WHERE c.id = magic_search_evidence.capture_id
             )),
             window_title = COALESCE(window_title, (
                 SELECT c.window_title FROM captures c WHERE c.id = magic_search_evidence.capture_id
             )),
             excerpt = COALESCE(excerpt, (
                 SELECT substr(c.content_text, 1, 220) FROM captures c WHERE c.id = magic_search_evidence.capture_id
             )),
             asset_paths_json = CASE WHEN asset_paths_json = '[]' THEN COALESCE((
                 SELECT json_group_array(ca.path) FROM capture_assets ca
                 WHERE ca.capture_id = magic_search_evidence.capture_id AND ca.path IS NOT NULL
             ), '[]') ELSE asset_paths_json END
         WHERE captured_at IS NULL
           AND EXISTS (SELECT 1 FROM captures c WHERE c.id = magic_search_evidence.capture_id);",
    )?;

    let now = now();
    conn.execute(
        "INSERT OR IGNORE INTO contexts (id, name, normalized_name, slug, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)",
        params!["inbox", "Inbox", "inbox", "inbox", now, now],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO contexts (id, name, normalized_name, slug, created_at, updated_at)
         VALUES (?, ?, 'knowledge base', ?, ?, ?)",
        params![
            "knowledge-base",
            "Knowledge Base",
            "knowledge-base",
            now,
            now
        ],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO settings (key, value) VALUES ('capture_screenshots', 'true')",
        [],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO settings (key, value) VALUES ('link_preview_fetch_enabled', 'true')",
        [],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO settings (key, value) VALUES ('ai_provider', 'deepseek')",
        [],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO settings (key, value) VALUES ('ai_model', 'deepseek-v4-flash')",
        [],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO settings (key, value) VALUES ('retention_policy', '3_months')",
        [],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO settings (key, value) VALUES ('retention_started_at', ?)",
        [now.as_str()],
    )?;
    for key in [
        "clipboard_monitor_enabled",
        "clipboard_monitor_capture_screenshots",
        "clipboard_monitor_quick_context_enabled",
    ] {
        conn.execute(
            "INSERT OR IGNORE INTO settings (key, value) VALUES (?, 'false')",
            [key],
        )?;
    }
    Ok(())
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for candidate in columns {
        if candidate? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn now() -> String {
    let now: DateTime<Utc> = Utc::now();
    now.to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::migrate;
    use rusqlite::Connection;

    #[test]
    fn migration_initializes_and_reopens_a_new_library() {
        let conn = Connection::open_in_memory().unwrap();

        migrate(&conn).unwrap();
        migrate(&conn).unwrap();

        let contexts = conn
            .prepare("SELECT id FROM contexts ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(contexts, ["inbox", "knowledge-base"]);

        let screenshots = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'capture_screenshots'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        assert_eq!(screenshots, "true");
        assert_eq!(
            conn.query_row(
                "SELECT value FROM settings WHERE key = 'link_preview_fetch_enabled'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
            "true"
        );
    }
}
