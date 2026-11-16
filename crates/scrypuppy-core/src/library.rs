//! Library read models and queries, independent of the windowing runtime.
use crate::clipboard;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashMap, path::Path};

const INBOX_CONTEXT_ID: &str = "inbox";
const KNOWLEDGE_BASE_CONTEXT_ID: &str = "knowledge-base";
fn err(error: impl ToString) -> String {
    error.to_string()
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ContextDto {
    pub id: String,
    pub name: String,
    pub normalized_name: String,
    pub slug: String,
    pub created_at: String,
    pub updated_at: String,
    pub capture_count: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ContextAssignmentDto {
    pub id: String,
    pub name: String,
    pub normalized_name: String,
    pub slug: String,
    pub created_at: String,
    pub updated_at: String,
    pub capture_count: i64,
    pub assignment_origin: String,
    pub confidence: Option<f64>,
    pub assigned_at: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CategoryDto {
    pub tag: String,
    pub capture_count: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CaptureAssetDto {
    pub id: String,
    pub kind: String,
    pub path: Option<String>,
    pub status: String,
    pub error: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CaptureRepresentationDto {
    pub id: String,
    pub kind: String,
    pub format_name: String,
    pub mime_type: Option<String>,
    pub text_content: Option<String>,
    pub asset_path: Option<String>,
    pub size_bytes: Option<i64>,
    pub sha256: Option<String>,
    pub restorable: bool,
    pub metadata: Value,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CaptureFileDto {
    pub id: String,
    pub representation_id: String,
    pub ordinal: i64,
    pub display_name: String,
    pub original_path: Option<String>,
    pub local_path: Option<String>,
    pub entry_kind: String,
    pub extension: Option<String>,
    pub size_bytes: Option<i64>,
    pub sha256: Option<String>,
    pub availability: String,
    pub metadata: Value,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CaptureClipboardFormatDto {
    pub id: i64,
    pub name: String,
    pub supported: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CaptureDto {
    pub id: String,
    pub content_text: String,
    pub captured_at: String,
    pub source_app_name: Option<String>,
    pub source_app_id: Option<String>,
    pub source_process_id: Option<i64>,
    pub source_process_path: Option<String>,
    pub window_title: Option<String>,
    pub window_id: Option<String>,
    pub platform: String,
    pub kind: String,
    pub content_kind: String,
    pub metadata: Value,
    pub assets: Vec<CaptureAssetDto>,
    pub representations: Vec<CaptureRepresentationDto>,
    pub files: Vec<CaptureFileDto>,
    pub clipboard_formats: Vec<CaptureClipboardFormatDto>,
    pub tags: Vec<String>,
    pub entities: Vec<CaptureEntityDto>,
    pub ocr: Option<CaptureOcrDto>,
    pub contexts: Vec<ContextAssignmentDto>,
}

#[derive(Debug, Serialize)]
pub struct CapturePageDto {
    pub items: Vec<CaptureDto>,
    pub total: usize,
}

#[derive(Debug, Deserialize)]
pub struct CaptureFilter {
    pub context_id: Option<String>,
    pub search: Option<String>,
    pub tag: Option<String>,
    #[serde(default)]
    pub app: Option<String>,
    #[serde(default)]
    pub date_from: Option<String>,
    #[serde(default)]
    pub date_to: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    #[serde(default)]
    pub exclude_references: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CaptureEntityDto {
    pub id: String,
    pub kind: String,
    pub value: String,
    pub source: String,
    pub confidence: f64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CaptureOcrDto {
    pub status: String,
    pub text: Option<String>,
    pub error: Option<String>,
    pub updated_at: String,
}

pub fn default_capture_filter() -> CaptureFilter {
    CaptureFilter {
        context_id: None,
        search: None,
        tag: None,
        app: None,
        date_from: None,
        date_to: None,
        limit: None,
        offset: None,
        exclude_references: false,
    }
}

struct CaptureQueryParts {
    where_clause: String,
    args: Vec<String>,
    search_pattern: Option<String>,
}

fn capture_query_parts(filter: &CaptureFilter) -> CaptureQueryParts {
    let mut args = Vec::new();
    let mut where_parts = Vec::new();

    if filter.exclude_references {
        where_parts.push("c.capture_kind <> 'reference'".to_string());
    }

    if let Some(context_id) = filter
        .context_id
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        if context_id == INBOX_CONTEXT_ID {
            where_parts.push("c.capture_kind <> 'reference' AND NOT EXISTS (SELECT 1 FROM capture_contexts icc WHERE icc.capture_id = c.id)".to_string());
        } else if context_id == KNOWLEDGE_BASE_CONTEXT_ID {
            where_parts.push("c.capture_kind = 'reference'".to_string());
        } else {
            where_parts.push("EXISTS (SELECT 1 FROM capture_contexts fcc WHERE fcc.capture_id = c.id AND fcc.context_id = ?)".to_string());
            args.push(context_id.clone());
        }
    }

    let mut search_pattern = None;
    if let Some(search) = filter
        .search
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        where_parts.push(
            "(c.content_text LIKE ?
              OR EXISTS (SELECT 1 FROM capture_representations sr WHERE sr.capture_id = c.id AND sr.text_content LIKE ?)
              OR EXISTS (SELECT 1 FROM capture_file_entries sf WHERE sf.capture_id = c.id AND (sf.display_name LIKE ? OR sf.extension LIKE ?))
              OR o.text LIKE ?
              OR c.source_app_name LIKE ?
              OR c.window_title LIKE ?
              OR EXISTS (SELECT 1 FROM capture_tags st WHERE st.capture_id = c.id AND st.tag LIKE ?)
              OR EXISTS (SELECT 1 FROM capture_entities se WHERE se.capture_id = c.id AND se.value LIKE ?)
              OR EXISTS (SELECT 1 FROM capture_contexts scc
                         JOIN contexts sco ON sco.id = scc.context_id
                         WHERE scc.capture_id = c.id AND sco.name LIKE ?))"
                .to_string(),
        );
        let pattern = format!("%{}%", search.trim());
        for _ in 0..10 {
            args.push(pattern.clone());
        }
        search_pattern = Some(pattern);
    }

    if let Some(tag) = filter.tag.as_ref().filter(|value| !value.trim().is_empty()) {
        where_parts.push(
            "EXISTS (SELECT 1 FROM capture_tags ct WHERE ct.capture_id = c.id AND ct.tag = ?)"
                .to_string(),
        );
        args.push(tag.clone());
    }

    if let Some(app) = filter.app.as_ref().filter(|value| !value.trim().is_empty()) {
        where_parts.push("lower(coalesce(c.source_app_name, '')) LIKE lower(?)".to_string());
        args.push(format!("%{}%", app.trim()));
    }

    if let Some(date_from) = filter
        .date_from
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        where_parts.push("c.captured_at >= ?".to_string());
        args.push(date_from.clone());
    }

    if let Some(date_to) = filter
        .date_to
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        where_parts.push("c.captured_at < ?".to_string());
        args.push(date_to.clone());
    }

    CaptureQueryParts {
        where_clause: if where_parts.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", where_parts.join(" AND "))
        },
        args,
        search_pattern,
    }
}

pub fn list_captures_from_conn(
    conn: &Connection,
    filter: &CaptureFilter,
) -> Result<Vec<CaptureDto>, String> {
    let limit = filter.limit.unwrap_or(200).clamp(1, 500);
    let offset = filter.offset.unwrap_or(0);

    let mut sql = String::from(
        "SELECT c.id, c.content_text, c.captured_at,
                c.source_app_name, c.source_app_id, c.source_process_id,
                c.source_process_path, c.window_title, c.window_id, c.platform,
                c.metadata_json, c.capture_kind
         FROM captures c
         LEFT JOIN capture_ocr o ON o.capture_id = c.id",
    );
    let parts = capture_query_parts(filter);
    sql.push_str(&parts.where_clause);
    let mut args = parts.args;

    if let Some(pattern) = parts.search_pattern {
        sql.push_str(
            " ORDER BY CASE
                WHEN lower(c.content_text) LIKE lower(?)
                  OR EXISTS (SELECT 1 FROM capture_representations rr WHERE rr.capture_id = c.id AND lower(rr.text_content) LIKE lower(?))
                  OR EXISTS (SELECT 1 FROM capture_file_entries rf WHERE rf.capture_id = c.id AND (lower(rf.display_name) LIKE lower(?) OR lower(rf.extension) LIKE lower(?))) THEN 5
                WHEN lower(o.text) LIKE lower(?) THEN 4
                WHEN lower(c.source_app_name) LIKE lower(?) OR lower(c.window_title) LIKE lower(?) THEN 3
                WHEN EXISTS (SELECT 1 FROM capture_tags ot WHERE ot.capture_id = c.id AND lower(ot.tag) LIKE lower(?))
                  OR EXISTS (SELECT 1 FROM capture_entities oe WHERE oe.capture_id = c.id AND lower(oe.value) LIKE lower(?)) THEN 2
                WHEN EXISTS (SELECT 1 FROM capture_contexts occ
                             JOIN contexts oco ON oco.id = occ.context_id
                             WHERE occ.capture_id = c.id AND lower(oco.name) LIKE lower(?)) THEN 1
                ELSE 0 END DESC, c.captured_at DESC",
        );
        for _ in 0..10 {
            args.push(pattern.clone());
        }
    } else {
        sql.push_str(" ORDER BY c.captured_at DESC");
    }
    sql.push_str(&format!(" LIMIT {limit} OFFSET {offset}"));

    let arg_refs: Vec<&dyn rusqlite::ToSql> = args
        .iter()
        .map(|value| value as &dyn rusqlite::ToSql)
        .collect();
    let mut stmt = conn.prepare(&sql).map_err(err)?;
    let mut captures = stmt
        .query_map(&arg_refs[..], capture_base_from_row)
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    hydrate_captures(conn, &mut captures)?;

    Ok(captures)
}

pub fn count_captures_from_conn(
    conn: &Connection,
    filter: &CaptureFilter,
) -> Result<usize, String> {
    let parts = capture_query_parts(filter);
    let sql = format!(
        "SELECT COUNT(DISTINCT c.id)
         FROM captures c
         LEFT JOIN capture_ocr o ON o.capture_id = c.id{}",
        parts.where_clause
    );
    let arg_refs: Vec<&dyn rusqlite::ToSql> = parts
        .args
        .iter()
        .map(|value| value as &dyn rusqlite::ToSql)
        .collect();
    let count = conn
        .query_row(&sql, &arg_refs[..], |row| row.get::<_, i64>(0))
        .map_err(err)?;
    Ok(count.max(0) as usize)
}

pub fn capture_base_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CaptureDto> {
    let id: String = row.get(0)?;
    let metadata_json: String = row.get(10)?;
    Ok(CaptureDto {
        id,
        content_text: row.get(1)?,
        captured_at: row.get(2)?,
        source_app_name: row.get(3)?,
        source_app_id: row.get(4)?,
        source_process_id: row.get(5)?,
        source_process_path: row.get(6)?,
        window_title: row.get(7)?,
        window_id: row.get(8)?,
        platform: row.get(9)?,
        kind: row.get(11)?,
        content_kind: "unknown".into(),
        metadata: serde_json::from_str(&metadata_json).unwrap_or_else(|_| json!({})),
        assets: Vec::new(),
        representations: Vec::new(),
        files: Vec::new(),
        clipboard_formats: Vec::new(),
        tags: Vec::new(),
        entities: Vec::new(),
        ocr: None,
        contexts: Vec::new(),
    })
}

pub fn hydrate_captures(conn: &Connection, captures: &mut [CaptureDto]) -> Result<(), String> {
    if captures.is_empty() {
        return Ok(());
    }
    let indexes = captures
        .iter()
        .enumerate()
        .map(|(index, capture)| (capture.id.clone(), index))
        .collect::<HashMap<_, _>>();
    let ids = captures
        .iter()
        .map(|capture| capture.id.clone())
        .collect::<Vec<_>>();
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let args = ids
        .iter()
        .map(|id| id as &dyn rusqlite::ToSql)
        .collect::<Vec<_>>();

    let mut stmt = conn
        .prepare(&format!(
            "SELECT capture_id, id, kind, path, status, error, created_at
         FROM capture_assets WHERE capture_id IN ({placeholders}) ORDER BY created_at"
        ))
        .map_err(err)?;
    let rows = stmt
        .query_map(&args[..], |row| {
            Ok((
                row.get::<_, String>(0)?,
                CaptureAssetDto {
                    id: row.get(1)?,
                    kind: row.get(2)?,
                    path: row.get(3)?,
                    status: row.get(4)?,
                    error: row.get(5)?,
                    created_at: row.get(6)?,
                },
            ))
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    drop(stmt);
    for (capture_id, asset) in rows {
        if let Some(index) = indexes.get(&capture_id) {
            captures[*index].assets.push(asset);
        }
    }

    let mut stmt = conn
        .prepare(&format!(
            "SELECT capture_id, id, kind, format_name, mime_type, text_content,
                    asset_path, size_bytes, sha256, restorable, metadata_json
             FROM capture_representations
             WHERE capture_id IN ({placeholders}) ORDER BY capture_id, ordinal"
        ))
        .map_err(err)?;
    let rows = stmt
        .query_map(&args[..], |row| {
            let metadata_json: String = row.get(10)?;
            Ok((
                row.get::<_, String>(0)?,
                CaptureRepresentationDto {
                    id: row.get(1)?,
                    kind: row.get(2)?,
                    format_name: row.get(3)?,
                    mime_type: row.get(4)?,
                    text_content: row.get(5)?,
                    asset_path: row.get(6)?,
                    size_bytes: row.get(7)?,
                    sha256: row.get(8)?,
                    restorable: row.get::<_, i64>(9)? != 0,
                    metadata: serde_json::from_str(&metadata_json).unwrap_or_else(|_| json!({})),
                },
            ))
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    drop(stmt);
    for (capture_id, representation) in rows {
        if let Some(index) = indexes.get(&capture_id) {
            captures[*index].representations.push(representation);
        }
    }

    let mut stmt = conn
        .prepare(&format!(
            "SELECT capture_id, id, representation_id, ordinal, display_name, original_path,
                    local_path, entry_kind, extension, size_bytes, sha256, availability,
                    metadata_json
             FROM capture_file_entries
             WHERE capture_id IN ({placeholders}) ORDER BY capture_id, ordinal"
        ))
        .map_err(err)?;
    let rows = stmt
        .query_map(&args[..], |row| {
            let metadata_json: String = row.get(12)?;
            Ok((
                row.get::<_, String>(0)?,
                CaptureFileDto {
                    id: row.get(1)?,
                    representation_id: row.get(2)?,
                    ordinal: row.get(3)?,
                    display_name: row.get(4)?,
                    original_path: row.get(5)?,
                    local_path: row.get(6)?,
                    entry_kind: row.get(7)?,
                    extension: row.get(8)?,
                    size_bytes: row.get(9)?,
                    sha256: row.get(10)?,
                    availability: row.get(11)?,
                    metadata: serde_json::from_str(&metadata_json).unwrap_or_else(|_| json!({})),
                },
            ))
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    drop(stmt);
    for (capture_id, file) in rows {
        if let Some(index) = indexes.get(&capture_id) {
            let mut file = file;
            if matches!(file.availability.as_str(), "available" | "missing") {
                if let Some(path) = file.local_path.as_deref().or(file.original_path.as_deref()) {
                    let path = Path::new(path);
                    if !clipboard::is_network_path(path) {
                        file.availability = if path.exists() {
                            "available".into()
                        } else {
                            "missing".into()
                        };
                    }
                }
            }
            captures[*index].files.push(file);
        }
    }

    let mut stmt = conn
        .prepare(&format!(
            "SELECT capture_id, format_id, format_name, supported
             FROM capture_clipboard_formats
             WHERE capture_id IN ({placeholders}) ORDER BY capture_id, format_id"
        ))
        .map_err(err)?;
    let rows = stmt
        .query_map(&args[..], |row| {
            Ok((
                row.get::<_, String>(0)?,
                CaptureClipboardFormatDto {
                    id: row.get(1)?,
                    name: row.get(2)?,
                    supported: row.get::<_, i64>(3)? != 0,
                },
            ))
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    drop(stmt);
    for (capture_id, format) in rows {
        if let Some(index) = indexes.get(&capture_id) {
            captures[*index].clipboard_formats.push(format);
        }
    }

    for capture in captures.iter_mut() {
        capture.content_kind = ["files", "image", "html", "rich_text", "url", "plain_text"]
            .into_iter()
            .find(|kind| {
                capture
                    .representations
                    .iter()
                    .any(|item| item.kind == *kind)
            })
            .unwrap_or("unknown")
            .to_string();
    }

    let mut stmt = conn
        .prepare(&format!(
        "SELECT capture_id, tag FROM capture_tags WHERE capture_id IN ({placeholders}) ORDER BY tag"
    ))
        .map_err(err)?;
    let rows = stmt
        .query_map(&args[..], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    drop(stmt);
    for (capture_id, tag) in rows {
        if let Some(index) = indexes.get(&capture_id) {
            captures[*index].tags.push(tag);
        }
    }

    let mut stmt = conn
        .prepare(&format!(
            "SELECT capture_id, id, kind, value, source, confidence FROM capture_entities
         WHERE capture_id IN ({placeholders}) ORDER BY kind, value"
        ))
        .map_err(err)?;
    let rows = stmt
        .query_map(&args[..], |row| {
            Ok((
                row.get::<_, String>(0)?,
                CaptureEntityDto {
                    id: row.get(1)?,
                    kind: row.get(2)?,
                    value: row.get(3)?,
                    source: row.get(4)?,
                    confidence: row.get(5)?,
                },
            ))
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    drop(stmt);
    for (capture_id, entity) in rows {
        if let Some(index) = indexes.get(&capture_id) {
            captures[*index].entities.push(entity);
        }
    }

    let mut stmt = conn
        .prepare(&format!(
            "SELECT capture_id, status, text, error, updated_at FROM capture_ocr
         WHERE capture_id IN ({placeholders})"
        ))
        .map_err(err)?;
    let rows = stmt
        .query_map(&args[..], |row| {
            Ok((
                row.get::<_, String>(0)?,
                CaptureOcrDto {
                    status: row.get(1)?,
                    text: row.get(2)?,
                    error: row.get(3)?,
                    updated_at: row.get(4)?,
                },
            ))
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    drop(stmt);
    for (capture_id, ocr) in rows {
        if let Some(index) = indexes.get(&capture_id) {
            captures[*index].ocr = Some(ocr);
        }
    }

    let mut stmt = conn
        .prepare(&format!(
            "SELECT cc.capture_id, co.id, co.name, co.normalized_name, co.slug,
                co.created_at, co.updated_at,
                (SELECT COUNT(*) FROM capture_contexts all_cc WHERE all_cc.context_id = co.id),
                cc.assignment_origin, cc.confidence, cc.created_at
         FROM capture_contexts cc
         JOIN contexts co ON co.id = cc.context_id
         WHERE cc.capture_id IN ({placeholders}) ORDER BY lower(co.name)"
        ))
        .map_err(err)?;
    let rows = stmt
        .query_map(&args[..], |row| {
            Ok((
                row.get::<_, String>(0)?,
                ContextAssignmentDto {
                    id: row.get(1)?,
                    name: row.get(2)?,
                    normalized_name: row.get(3)?,
                    slug: row.get(4)?,
                    created_at: row.get(5)?,
                    updated_at: row.get(6)?,
                    capture_count: row.get(7)?,
                    assignment_origin: row.get(8)?,
                    confidence: row.get(9)?,
                    assigned_at: row.get(10)?,
                },
            ))
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    for (capture_id, context) in rows {
        if let Some(index) = indexes.get(&capture_id) {
            captures[*index].contexts.push(context);
        }
    }
    Ok(())
}

pub fn list_contexts_from_conn(conn: &Connection) -> Result<Vec<ContextDto>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT co.id, co.name, co.normalized_name, co.slug, co.created_at, co.updated_at,
                    COUNT(cc.capture_id) AS capture_count
             FROM contexts co
             LEFT JOIN capture_contexts cc ON cc.context_id = co.id
             WHERE co.id NOT IN ('inbox', 'knowledge-base')
             GROUP BY co.id
             ORDER BY lower(co.name)",
        )
        .map_err(err)?;

    let contexts = stmt
        .query_map([], |row| {
            Ok(ContextDto {
                id: row.get(0)?,
                name: row.get(1)?,
                normalized_name: row.get(2)?,
                slug: row.get(3)?,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
                capture_count: row.get(6)?,
            })
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    Ok(contexts)
}

pub fn get_capture_by_id(conn: &Connection, id: &str) -> Result<CaptureDto, String> {
    let capture = conn
        .query_row(
            "SELECT c.id, c.content_text, c.captured_at,
                c.source_app_name, c.source_app_id, c.source_process_id,
                c.source_process_path, c.window_title, c.window_id, c.platform,
                c.metadata_json, c.capture_kind
         FROM captures c
         WHERE c.id = ?",
            [id],
            capture_base_from_row,
        )
        .map_err(err)?;
    let mut captures = vec![capture];
    hydrate_captures(conn, &mut captures)?;
    captures
        .pop()
        .ok_or_else(|| "Captura nao encontrada.".into())
}

#[cfg(test)]
mod tests {
    use super::{capture_query_parts, CaptureFilter};

    #[test]
    fn capture_filters_add_app_and_exclusive_date_bounds() {
        let filter = CaptureFilter {
            context_id: None,
            search: None,
            tag: Some("release".into()),
            app: Some("Visual Studio".into()),
            date_from: Some("2026-11-01T00:00:00".into()),
            date_to: Some("2026-12-01T00:00:00".into()),
            limit: None,
            offset: None,
            exclude_references: false,
        };

        let parts = capture_query_parts(&filter);

        assert!(parts.where_clause.contains("ct.tag = ?"));
        assert!(parts
            .where_clause
            .contains("lower(coalesce(c.source_app_name, '')) LIKE lower(?)"));
        assert!(parts.where_clause.contains("c.captured_at >= ?"));
        assert!(parts.where_clause.contains("c.captured_at < ?"));
        assert_eq!(
            parts.args,
            [
                "release",
                "%Visual Studio%",
                "2026-11-01T00:00:00",
                "2026-12-01T00:00:00"
            ]
        );
    }

    #[test]
    fn blank_app_and_date_filters_are_ignored() {
        let filter = CaptureFilter {
            context_id: None,
            search: None,
            tag: None,
            app: Some("  ".into()),
            date_from: Some(String::new()),
            date_to: None,
            limit: None,
            offset: None,
            exclude_references: false,
        };

        let parts = capture_query_parts(&filter);

        assert!(parts.where_clause.is_empty());
        assert!(parts.args.is_empty());
    }
}
