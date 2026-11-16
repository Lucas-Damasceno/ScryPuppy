//! Persistent, source-linked notes created from library answers.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::assistant::ChatAnswer;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DocumentListItem {
    pub id: String,
    pub root_id: String,
    pub version: i64,
    pub title: String,
    pub query: String,
    pub created_at: String,
    pub evidence_count: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DocumentEvidence {
    pub capture_id: String,
    pub captured_at: String,
    pub app_name: Option<String>,
    pub excerpt: String,
    pub available: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Document {
    pub id: String,
    pub root_id: String,
    pub version: i64,
    pub title: String,
    pub query: String,
    pub markdown: String,
    pub created_at: String,
    pub evidence: Vec<DocumentEvidence>,
}

/// Saves an evidence-grounded assistant response as a versioned library document.
pub fn save_answer(
    conn: &Connection,
    query: &str,
    answer: &ChatAnswer,
    title: Option<&str>,
) -> Result<Document, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("A pergunta do documento não pode ficar vazia.".into());
    }
    if answer.answer.trim().is_empty() {
        return Err("O documento não pode ficar vazio.".into());
    }
    let title = title
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(query)
        .chars()
        .take(120)
        .collect::<String>();
    let root_id = Uuid::new_v4().to_string();
    let id = Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let markdown = format!(
        "# {title}\n\n**Pergunta:** {query}\n\n{}\n\n## Fontes\n{}",
        answer.answer.trim(),
        if answer.evidence.is_empty() {
            "Nenhuma captura foi citada.".to_string()
        } else {
            answer
                .evidence
                .iter()
                .enumerate()
                .map(|(index, evidence)| {
                    format!(
                        "{}. {} — {} ({})",
                        index + 1,
                        evidence
                            .app_name
                            .as_deref()
                            .unwrap_or("Aplicativo desconhecido"),
                        evidence.excerpt.trim(),
                        evidence.captured_at
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
    );
    // Keep the filter payload compatible with the existing Tauri Magic Search
    // reader, which still consumes these rows during the migration period.
    let filters = serde_json::json!({
        "query": query,
        "context_ids": [],
        "include_knowledge_base": false,
        "include_inbox": false,
        "response_mode": "direct"
    });
    conn.execute(
        "INSERT INTO magic_search_documents
         (id, root_id, previous_document_id, version, title, query, markdown, provider, model,
          filters_json, generation_warning, evidence_count, created_at)
         VALUES (?1, ?2, NULL, 1, ?3, ?4, ?5, 'local', 'evidence-assistant', ?6, NULL, ?7, ?8)",
        params![
            id,
            root_id,
            title,
            query,
            markdown,
            filters.to_string(),
            answer.evidence.len() as i64,
            created_at
        ],
    )
    .map_err(|error| error.to_string())?;
    for (rank, evidence) in answer.evidence.iter().enumerate() {
        conn.execute(
            "INSERT OR IGNORE INTO magic_search_evidence
             (document_id, capture_id, rank, score, captured_at, context_name, app_name,
              application_id, window_title, excerpt, asset_paths_json)
             VALUES (?1, ?2, ?3, 0, ?4, ?5, ?6, ?7, ?8, ?9, '[]')",
            params![
                id,
                evidence.capture_id,
                rank as i64,
                evidence.captured_at,
                evidence.context_names.join(", "),
                evidence.app_name,
                evidence.application_id,
                evidence.window_title,
                evidence.excerpt,
            ],
        )
        .map_err(|error| error.to_string())?;
    }
    get(conn, &id)
}

pub fn list(conn: &Connection) -> Result<Vec<DocumentListItem>, String> {
    let exists = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'magic_search_documents')",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|error| error.to_string())?;
    if !exists {
        return Ok(Vec::new());
    }
    let mut statement = conn
        .prepare(
            "SELECT id, root_id, version, title, query, created_at, evidence_count
             FROM magic_search_documents ORDER BY created_at DESC LIMIT 100",
        )
        .map_err(|error| error.to_string())?;
    let documents = statement
        .query_map([], |row| {
            Ok(DocumentListItem {
                id: row.get(0)?,
                root_id: row.get(1)?,
                version: row.get(2)?,
                title: row.get(3)?,
                query: row.get(4)?,
                created_at: row.get(5)?,
                evidence_count: row.get(6)?,
            })
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    Ok(documents)
}

pub fn get(conn: &Connection, id: &str) -> Result<Document, String> {
    let mut document = conn
        .query_row(
            "SELECT id, root_id, version, title, query, markdown, created_at
             FROM magic_search_documents WHERE id = ?1",
            [id],
            |row| {
                Ok(Document {
                    id: row.get(0)?,
                    root_id: row.get(1)?,
                    version: row.get(2)?,
                    title: row.get(3)?,
                    query: row.get(4)?,
                    markdown: row.get(5)?,
                    created_at: row.get(6)?,
                    evidence: Vec::new(),
                })
            },
        )
        .map_err(|error| error.to_string())?;
    let mut statement = conn
        .prepare(
            "SELECT evidence.capture_id, COALESCE(evidence.captured_at, ''), evidence.app_name,
                    COALESCE(evidence.excerpt, ''), captures.id IS NOT NULL
             FROM magic_search_evidence evidence
             LEFT JOIN captures ON captures.id = evidence.capture_id
             WHERE evidence.document_id = ?1 ORDER BY evidence.rank",
        )
        .map_err(|error| error.to_string())?;
    document.evidence = statement
        .query_map([id], |row| {
            Ok(DocumentEvidence {
                capture_id: row.get(0)?,
                captured_at: row.get(1)?,
                app_name: row.get(2)?,
                excerpt: row.get(3)?,
                available: row.get(4)?,
            })
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    Ok(document)
}

pub fn update_markdown(conn: &Connection, id: &str, markdown: &str) -> Result<(), String> {
    if markdown.trim().is_empty() {
        return Err("O documento não pode ficar vazio.".into());
    }
    let changed = conn
        .execute(
            "UPDATE magic_search_documents SET markdown = ?1 WHERE id = ?2",
            params![markdown, id],
        )
        .map_err(|error| error.to_string())?;
    if changed == 0 {
        return Err("Documento não encontrado.".into());
    }
    Ok(())
}

pub fn delete_root(conn: &Connection, root_id: &str) -> Result<(), String> {
    conn.execute(
        "DELETE FROM magic_search_documents WHERE root_id = ?1",
        [root_id],
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::EvidenceItem;

    #[test]
    fn saved_answer_keeps_source_links_and_can_be_edited() {
        let conn = Connection::open_in_memory().unwrap();
        crate::schema::migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO contexts (id, name, slug, created_at, updated_at)
             VALUES ('work', 'Trabalho', 'trabalho', 'now', 'now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO captures
             (id, context_id, content_text, content_hash, captured_at, platform, metadata_json, capture_kind, created_at)
             VALUES ('source-1', 'work', 'Prazo sexta-feira', 'hash', '2026-11-01', 'windows', '{}', 'capture', 'now')",
            [],
        )
        .unwrap();
        let answer = ChatAnswer {
            answer: "O prazo é sexta-feira.".into(),
            confidence: "alta".into(),
            evidence: vec![EvidenceItem {
                capture_id: "source-1".into(),
                captured_at: "2026-11-01T12:00:00Z".into(),
                context_names: vec!["Trabalho".into()],
                app_name: Some("Editor".into()),
                application_id: None,
                window_title: Some("Planejamento".into()),
                excerpt: "Prazo: sexta-feira".into(),
                matched_fields: vec!["text".into()],
            }],
        };
        let document = save_answer(&conn, "Qual é o prazo?", &answer, None).unwrap();
        let stored_filters: serde_json::Value = conn
            .query_row(
                "SELECT filters_json FROM magic_search_documents WHERE id = ?1",
                [&document.id],
                |row| row.get::<_, String>(0),
            )
            .map(|value| serde_json::from_str(&value).unwrap())
            .unwrap();
        assert_eq!(stored_filters["query"], "Qual é o prazo?");
        assert_eq!(stored_filters["response_mode"], "direct");
        assert_eq!(document.evidence.len(), 1);
        assert_eq!(document.evidence[0].capture_id, "source-1");
        assert!(document.evidence[0].available);
        assert!(document.markdown.contains("## Fontes"));
        update_markdown(&conn, &document.id, "# Revisado").unwrap();
        assert_eq!(get(&conn, &document.id).unwrap().markdown, "# Revisado");
        assert_eq!(list(&conn).unwrap().len(), 1);
        conn.execute("DELETE FROM captures WHERE id = 'source-1'", [])
            .unwrap();
        assert!(!get(&conn, &document.id).unwrap().evidence[0].available);
        delete_root(&conn, &document.root_id).unwrap();
        assert!(list(&conn).unwrap().is_empty());
    }
}
