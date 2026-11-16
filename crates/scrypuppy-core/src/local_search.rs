use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};

use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use rusqlite::{params, Connection};
use serde::Serialize;

use crate::app_error::AppNotice;

pub const MODEL_ID: &str = "intfloat/multilingual-e5-small";
pub const MODEL_NAME: &str = "Multilingual E5 Small";
pub const MODEL_DIMENSIONS: usize = 384;
pub const INDEX_VERSION: i64 = 1;
const INSTALL_MARKER: &str = "installed.json";

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LocalSearchPhase {
    NotDownloaded,
    Downloading,
    Indexing,
    Ready,
    Error,
    Removing,
}

#[derive(Clone, Debug, Serialize)]
pub struct LocalSearchStatus {
    pub phase: LocalSearchPhase,
    pub model_id: String,
    pub model_name: String,
    pub cache_bytes: u64,
    pub indexed_count: usize,
    pub total_count: usize,
    pub pending_count: usize,
    pub error: Option<AppNotice>,
    pub can_download: bool,
    pub can_retry: bool,
    pub can_remove: bool,
}

impl LocalSearchStatus {
    fn initial(installed: bool, cache_bytes: u64) -> Self {
        Self {
            phase: if installed {
                LocalSearchPhase::Indexing
            } else {
                LocalSearchPhase::NotDownloaded
            },
            model_id: MODEL_ID.into(),
            model_name: MODEL_NAME.into(),
            cache_bytes,
            indexed_count: 0,
            total_count: 0,
            pending_count: 0,
            error: None,
            can_download: !installed,
            can_retry: false,
            can_remove: installed || cache_bytes > 0,
        }
    }
}

pub struct LocalSearchManager {
    cache_dir: PathBuf,
    model: Mutex<Option<TextEmbedding>>,
    status: Mutex<LocalSearchStatus>,
    operation_running: AtomicBool,
    cancel_requested: AtomicBool,
}

impl LocalSearchManager {
    pub fn new(app_dir: &Path) -> Self {
        let cache_dir = app_dir.join("models").join("fastembed");
        let installed = cache_dir.join(INSTALL_MARKER).is_file();
        let cache_bytes = directory_size(&cache_dir).unwrap_or_default();
        Self {
            cache_dir,
            model: Mutex::new(None),
            status: Mutex::new(LocalSearchStatus::initial(installed, cache_bytes)),
            operation_running: AtomicBool::new(false),
            cancel_requested: AtomicBool::new(false),
        }
    }

    pub fn status(&self) -> LocalSearchStatus {
        self.status
            .lock()
            .map(|value| value.clone())
            .unwrap_or_else(|_| LocalSearchStatus::initial(false, 0))
    }

    pub fn installed(&self) -> bool {
        self.cache_dir.join(INSTALL_MARKER).is_file()
    }

    pub fn try_begin_operation(&self) -> bool {
        self.cancel_requested.store(false, Ordering::Release);
        self.operation_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub fn finish_operation(&self) {
        self.operation_running.store(false, Ordering::Release);
    }

    pub fn operation_running(&self) -> bool {
        self.operation_running.load(Ordering::Acquire)
    }

    pub fn request_cancel(&self) {
        self.cancel_requested.store(true, Ordering::Release);
    }

    pub fn cancel_requested(&self) -> bool {
        self.cancel_requested.load(Ordering::Acquire)
    }

    pub fn set_phase(&self, phase: LocalSearchPhase, error: Option<AppNotice>) {
        if let Ok(mut status) = self.status.lock() {
            status.phase = phase.clone();
            status.error = error;
            status.cache_bytes = directory_size(&self.cache_dir).unwrap_or_default();
            status.can_download = phase == LocalSearchPhase::NotDownloaded;
            status.can_retry = phase == LocalSearchPhase::Error;
            status.can_remove = self.installed() || status.cache_bytes > 0;
        }
    }

    pub fn set_index_progress(&self, indexed: usize, total: usize) {
        if let Ok(mut status) = self.status.lock() {
            status.indexed_count = indexed;
            status.total_count = total;
            status.pending_count = total.saturating_sub(indexed);
        }
    }

    pub fn load_model(&self, allow_download: bool) -> Result<(), String> {
        if !allow_download && !self.installed() {
            return Err("local model is not installed".into());
        }
        fs::create_dir_all(&self.cache_dir).map_err(|error| error.to_string())?;
        let options = TextInitOptions::new(EmbeddingModel::MultilingualE5Small)
            .with_cache_dir(self.cache_dir.clone())
            .with_show_download_progress(false)
            .with_intra_threads(4);
        let model = TextEmbedding::try_new(options).map_err(|error| error.to_string())?;
        let mut guard = self
            .model
            .lock()
            .map_err(|_| "local model lock is unavailable".to_string())?;
        *guard = Some(model);
        Ok(())
    }

    pub fn mark_installed(&self) -> Result<(), String> {
        fs::create_dir_all(&self.cache_dir).map_err(|error| error.to_string())?;
        let temporary = self.cache_dir.join("installed.json.tmp");
        let destination = self.cache_dir.join(INSTALL_MARKER);
        let mut file = fs::File::create(&temporary).map_err(|error| error.to_string())?;
        file.write_all(
            format!(
                "{{\"model_id\":\"{}\",\"index_version\":{}}}",
                MODEL_ID, INDEX_VERSION
            )
            .as_bytes(),
        )
        .map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        fs::rename(temporary, destination).map_err(|error| error.to_string())
    }

    pub fn embed_query(&self, query: &str) -> Result<Vec<f32>, String> {
        self.embed(vec![format!("query: {}", query.trim())])?
            .into_iter()
            .next()
            .ok_or_else(|| "the local model returned no query embedding".into())
    }

    pub fn embed_passages(&self, passages: Vec<String>) -> Result<Vec<Vec<f32>>, String> {
        self.embed(
            passages
                .into_iter()
                .map(|value| format!("passage: {value}"))
                .collect(),
        )
    }

    fn embed(&self, values: Vec<String>) -> Result<Vec<Vec<f32>>, String> {
        let mut guard = self
            .model
            .lock()
            .map_err(|_| "local model lock is unavailable".to_string())?;
        let model = guard
            .as_mut()
            .ok_or_else(|| "local model is not loaded".to_string())?;
        let embeddings = model
            .embed(values, Some(32))
            .map_err(|error| error.to_string())?;
        for embedding in &embeddings {
            if embedding.len() != MODEL_DIMENSIONS {
                return Err(format!(
                    "unexpected embedding dimensions: expected {MODEL_DIMENSIONS}, got {}",
                    embedding.len()
                ));
            }
        }
        Ok(embeddings)
    }

    pub fn remove_model(&self) -> Result<(), String> {
        let mut guard = self
            .model
            .lock()
            .map_err(|_| "local model lock is unavailable".to_string())?;
        *guard = None;
        drop(guard);
        if self.cache_dir.exists() {
            fs::remove_dir_all(&self.cache_dir).map_err(|error| error.to_string())?;
        }
        self.set_index_progress(0, 0);
        self.set_phase(LocalSearchPhase::NotDownloaded, None);
        Ok(())
    }
}

/// Rebuild the canonical search documents before vector indexing and remove
/// rows that no longer have a corresponding capture.
pub fn reconcile_search_documents(conn: &Connection) -> Result<usize, String> {
    let mut stmt = conn
        .prepare("SELECT id FROM captures ORDER BY captured_at ASC")
        .map_err(|error| error.to_string())?;
    let ids = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    drop(stmt);

    let updated_at = chrono::Utc::now().to_rfc3339();
    for capture_id in &ids {
        crate::search_index::refresh_search_document(conn, capture_id, &updated_at)?;
    }
    conn.execute(
        "DELETE FROM capture_search_documents
         WHERE NOT EXISTS (SELECT 1 FROM captures WHERE captures.id = capture_search_documents.capture_id)",
        [],
    )
    .map_err(|error| error.to_string())?;
    Ok(ids.len())
}

/// Index stale or missing capture vectors in bounded batches. The callback is
/// invoked after each committed batch so UI shells can publish live progress.
pub fn index_embeddings(
    conn: &mut Connection,
    manager: &LocalSearchManager,
    mut on_progress: impl FnMut(LocalSearchStatus),
) -> Result<(), String> {
    let total = reconcile_search_documents(conn)?;
    let mut stmt = conn
        .prepare(
            "SELECT d.capture_id, trim(d.content || '\n' || d.ocr || '\n' || d.metadata), d.content_hash
             FROM capture_search_documents d
             LEFT JOIN capture_embeddings e ON e.capture_id = d.capture_id
               AND e.model_id = ? AND e.index_version = ? AND e.dimensions = ?
               AND e.content_hash = d.content_hash
             WHERE e.capture_id IS NULL
             ORDER BY d.id ASC",
        )
        .map_err(|error| error.to_string())?;
    let pending = stmt
        .query_map(
            params![MODEL_ID, INDEX_VERSION, MODEL_DIMENSIONS as i64],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    drop(stmt);

    let mut indexed = total.saturating_sub(pending.len());
    manager.set_index_progress(indexed, total);
    on_progress(manager.status());

    for batch in pending.chunks(32) {
        if manager.cancel_requested() {
            return Err("local indexing was cancelled".into());
        }
        let embeddings = manager.embed_passages(
            batch
                .iter()
                .map(|(_, passage, _)| passage.clone())
                .collect(),
        )?;
        let transaction = conn.transaction().map_err(|error| error.to_string())?;
        for ((capture_id, _, content_hash), embedding) in batch.iter().zip(embeddings) {
            let vector = encode_embedding(&normalize_embedding(embedding))?;
            transaction
                .execute(
                    "INSERT INTO capture_embeddings
                     (capture_id, model_id, index_version, dimensions, content_hash, vector, updated_at)
                     VALUES (?, ?, ?, ?, ?, ?, ?)
                     ON CONFLICT(capture_id) DO UPDATE SET
                        model_id = excluded.model_id,
                        index_version = excluded.index_version,
                        dimensions = excluded.dimensions,
                        content_hash = excluded.content_hash,
                        vector = excluded.vector,
                        updated_at = excluded.updated_at",
                    params![
                        capture_id,
                        MODEL_ID,
                        INDEX_VERSION,
                        MODEL_DIMENSIONS as i64,
                        content_hash,
                        vector,
                        chrono::Utc::now().to_rfc3339()
                    ],
                )
                .map_err(|error| error.to_string())?;
        }
        transaction.commit().map_err(|error| error.to_string())?;
        indexed += batch.len();
        manager.set_index_progress(indexed, total);
        on_progress(manager.status());
    }
    Ok(())
}

/// Rank captures with the same lexical + E5 reciprocal-rank fusion used by
/// Magic Search. If the local model is unavailable, the canonical FTS ranking
/// is returned by itself.
pub fn rank_capture_ids(
    conn: &Connection,
    manager: &LocalSearchManager,
    query: &str,
) -> Result<Vec<String>, String> {
    let terms = query
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| term.chars().count() > 1)
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .take(24)
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let fts_query = terms.join(" OR ");
    let mut rankings = Vec::<(&str, Vec<String>)>::new();
    let mut lexical = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT d.capture_id FROM capture_search_fts
         JOIN capture_search_documents d ON d.id = capture_search_fts.rowid
         WHERE capture_search_fts MATCH ?
         ORDER BY bm25(capture_search_fts, 5.0, 4.0, 2.0), d.capture_id LIMIT 200",
    ) {
        if let Ok(rows) = stmt.query_map([fts_query], |row| row.get::<_, String>(0)) {
            lexical.extend(rows.filter_map(Result::ok));
        }
    }
    if !lexical.is_empty() {
        rankings.push(("lexical", lexical));
    }

    if manager.status().phase == LocalSearchPhase::Ready {
        if let Ok(query_vector) = manager.embed_query(query) {
            let query_vector = normalize_embedding(query_vector);
            if let Ok(mut stmt) = conn.prepare(
                "SELECT e.capture_id, e.vector FROM capture_embeddings e
                 JOIN capture_search_documents d ON d.capture_id = e.capture_id
                    AND d.content_hash = e.content_hash
                 WHERE e.model_id = ? AND e.index_version = ? AND e.dimensions = ?",
            ) {
                if let Ok(rows) = stmt.query_map(
                    params![MODEL_ID, INDEX_VERSION, MODEL_DIMENSIONS as i64],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
                ) {
                    let mut semantic = rows
                        .filter_map(Result::ok)
                        .filter_map(|(id, bytes)| {
                            decode_embedding(&bytes)
                                .map(|vector| (id, cosine_similarity(&query_vector, &vector)))
                        })
                        .collect::<Vec<_>>();
                    semantic.sort_by(|left, right| {
                        right
                            .1
                            .total_cmp(&left.1)
                            .then_with(|| left.0.cmp(&right.0))
                    });
                    let semantic = semantic
                        .into_iter()
                        .take(200)
                        .map(|(id, _)| id)
                        .collect::<Vec<_>>();
                    if !semantic.is_empty() {
                        rankings.push(("semantic", semantic));
                    }
                }
            }
        }
    }

    let mut fused = HashMap::<String, f64>::new();
    for (_, ranking) in rankings {
        for (rank, id) in ranking.iter().enumerate() {
            *fused.entry(id.clone()).or_default() += 1.0 / (60.0 + (rank + 1) as f64);
        }
    }
    let mut ranked = fused.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    Ok(ranked.into_iter().map(|(id, _)| id).collect())
}

pub fn normalize_embedding(mut embedding: Vec<f32>) -> Vec<f32> {
    let norm = embedding
        .iter()
        .map(|value| value * value)
        .sum::<f32>()
        .sqrt();
    if norm > f32::EPSILON {
        for value in &mut embedding {
            *value /= norm;
        }
    }
    embedding
}

pub fn encode_embedding(embedding: &[f32]) -> Result<Vec<u8>, String> {
    if embedding.len() != MODEL_DIMENSIONS || embedding.iter().any(|value| !value.is_finite()) {
        return Err("invalid local embedding".into());
    }
    let mut bytes = Vec::with_capacity(std::mem::size_of_val(embedding));
    for value in embedding {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    Ok(bytes)
}

pub fn decode_embedding(bytes: &[u8]) -> Option<Vec<f32>> {
    if bytes.len() != MODEL_DIMENSIONS * std::mem::size_of::<f32>() {
        return None;
    }
    let values = bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect::<Vec<_>>();
    values
        .iter()
        .all(|value| value.is_finite())
        .then_some(values)
}

pub fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    if left.len() != right.len() {
        return f32::NEG_INFINITY;
    }
    left.iter().zip(right).map(|(a, b)| a * b).sum()
}

fn directory_size(path: &Path) -> std::io::Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    let mut total = 0u64;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        total = total.saturating_add(if metadata.is_dir() {
            directory_size(&entry.path())?
        } else {
            metadata.len()
        });
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vectors_round_trip_in_little_endian_format() {
        let source = vec![0.25; MODEL_DIMENSIONS];
        let encoded = encode_embedding(&source).expect("valid embedding");
        assert_eq!(encoded.len(), MODEL_DIMENSIONS * 4);
        assert_eq!(decode_embedding(&encoded), Some(source));
    }

    #[test]
    fn normalization_produces_unit_length() {
        let normalized = normalize_embedding(vec![3.0, 4.0]);
        let length = normalized
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt();
        assert!((length - 1.0).abs() < 0.0001);
    }

    #[test]
    fn cosine_rejects_mismatched_vectors() {
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 2.0]), f32::NEG_INFINITY);
    }

    #[test]
    fn lexical_rank_uses_the_canonical_fts_index_without_a_model() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE capture_search_documents (
                id INTEGER PRIMARY KEY, capture_id TEXT NOT NULL, content_hash TEXT NOT NULL
             );
             CREATE VIRTUAL TABLE capture_search_fts USING fts5(content, ocr, metadata);
             INSERT INTO capture_search_documents VALUES (1, 'first', 'hash-1');
             INSERT INTO capture_search_documents VALUES (2, 'second', 'hash-2');
             INSERT INTO capture_search_fts(rowid, content, ocr, metadata)
                VALUES (1, 'invoice payment due', '', '');
             INSERT INTO capture_search_fts(rowid, content, ocr, metadata)
                VALUES (2, 'meeting notes', '', '');",
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let manager = LocalSearchManager::new(directory.path());

        let ranked = rank_capture_ids(&conn, &manager, "invoice payment").unwrap();

        assert_eq!(ranked, ["first"]);
    }
}
