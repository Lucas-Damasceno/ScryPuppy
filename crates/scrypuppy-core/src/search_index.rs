//! Canonical text indexing shared by all ScryPuppy UI surfaces.
use rusqlite::{params, Connection};

use crate::{
    crypto::sha256_hex,
    library::{get_capture_by_id, CaptureDto},
};

fn err(error: impl ToString) -> String {
    error.to_string()
}

pub struct CanonicalSearchDocument {
    pub content: String,
    pub ocr: String,
    pub metadata: String,
    pub content_hash: String,
}

fn bounded_search_text(value: &str) -> String {
    value.chars().take(32_000).collect()
}

pub fn canonical_search_document(capture: &CaptureDto) -> CanonicalSearchDocument {
    let mut seen = std::collections::HashSet::new();
    let mut content_parts = Vec::new();
    for value in std::iter::once(Some(capture.content_text.as_str())).chain(
        capture
            .representations
            .iter()
            .map(|representation| representation.text_content.as_deref()),
    ) {
        let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
            continue;
        };
        if seen.insert(value.to_string()) {
            content_parts.push(value);
        }
    }
    let content = bounded_search_text(&content_parts.join("\n"));
    let ocr = bounded_search_text(
        capture
            .ocr
            .as_ref()
            .and_then(|v| v.text.as_deref())
            .unwrap_or_default(),
    );
    let metadata = bounded_search_text(
        &[
            capture
                .source_app_name
                .as_deref()
                .unwrap_or_default()
                .to_string(),
            capture
                .source_app_id
                .as_deref()
                .unwrap_or_default()
                .to_string(),
            capture
                .window_title
                .as_deref()
                .unwrap_or_default()
                .to_string(),
            capture
                .contexts
                .iter()
                .map(|context| context.name.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            capture.tags.join(" "),
            capture
                .entities
                .iter()
                .map(|entity| format!("{} {}", entity.kind, entity.value))
                .collect::<Vec<_>>()
                .join(" "),
            capture
                .files
                .iter()
                .map(|file| {
                    format!(
                        "{} {} {}",
                        file.display_name,
                        file.extension.as_deref().unwrap_or_default(),
                        file.original_path.as_deref().unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>()
                .join(" "),
        ]
        .join("\n"),
    );
    let content_hash = sha256_hex(format!("{content}\0{ocr}\0{metadata}").as_bytes());
    CanonicalSearchDocument {
        content,
        ocr,
        metadata,
        content_hash,
    }
}

/// Refresh the FTS source row and invalidate semantic vectors when content changes.
pub fn refresh_search_document(
    conn: &Connection,
    capture_id: &str,
    updated_at: &str,
) -> Result<(), String> {
    let exists = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM captures WHERE id = ?)",
            [capture_id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(err)?
        != 0;
    if !exists {
        conn.execute(
            "DELETE FROM capture_search_documents WHERE capture_id = ?",
            [capture_id],
        )
        .map_err(err)?;
        return Ok(());
    }
    let capture = get_capture_by_id(conn, capture_id)?;
    let document = canonical_search_document(&capture);
    conn.execute(
        "INSERT INTO capture_search_documents
         (capture_id, content, ocr, metadata, content_hash, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(capture_id) DO UPDATE SET
            content = excluded.content, ocr = excluded.ocr, metadata = excluded.metadata,
            content_hash = excluded.content_hash, updated_at = excluded.updated_at",
        params![
            capture_id,
            document.content,
            document.ocr,
            document.metadata,
            document.content_hash,
            updated_at
        ],
    )
    .map_err(err)?;
    conn.execute(
        "DELETE FROM capture_embeddings WHERE capture_id = ? AND content_hash <> (
            SELECT content_hash FROM capture_search_documents WHERE capture_id = ?
         )",
        params![capture_id, capture_id],
    )
    .map_err(err)?;
    Ok(())
}
