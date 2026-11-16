//! Local OCR queue shared by the desktop front ends.

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};

pub fn enqueue_pending_jobs(conn: &Connection) -> Result<usize, String> {
    let mut stmt = conn
        .prepare(
            "SELECT c.id, a.id FROM captures c
             JOIN capture_assets a ON a.capture_id = c.id
             WHERE a.kind IN ('clipboard_image', 'imported_image', 'screenshot')
               AND a.status = 'saved' AND a.path IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM ocr_jobs j WHERE j.capture_id = c.id)",
        )
        .map_err(err)?;
    let jobs = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(err)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(err)?;
    drop(stmt);
    let at = chrono::Utc::now().to_rfc3339();
    for (capture_id, asset_id) in &jobs {
        conn.execute(
            "INSERT OR IGNORE INTO ocr_jobs (capture_id, asset_id, status, queued_at)
             VALUES (?1, ?2, 'queued', ?3)",
            params![capture_id, asset_id, at],
        )
        .map_err(err)?;
        conn.execute(
            "INSERT OR IGNORE INTO capture_ocr (capture_id, status, updated_at)
             VALUES (?1, 'queued', ?2)",
            params![capture_id, at],
        )
        .map_err(err)?;
    }
    Ok(jobs.len())
}

pub fn recover_interrupted_jobs(conn: &Connection) -> Result<(), String> {
    let at = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE ocr_jobs SET status = 'queued', started_at = NULL
         WHERE status = 'running'",
        [],
    )
    .map_err(err)?;
    conn.execute(
        "UPDATE capture_ocr SET status = 'queued', error = NULL, updated_at = ?1
         WHERE status = 'running'",
        [at],
    )
    .map_err(err)?;
    Ok(())
}

pub fn enqueue_capture(conn: &Connection, capture_id: &str) -> Result<bool, String> {
    let asset_id: Option<String> = conn
        .query_row(
            "SELECT id FROM capture_assets WHERE capture_id = ?1
             AND kind IN ('clipboard_image', 'imported_image', 'screenshot')
             AND status = 'saved' AND path IS NOT NULL
             ORDER BY created_at LIMIT 1",
            [capture_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(err)?;
    let Some(asset_id) = asset_id else {
        return Ok(false);
    };
    let at = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT OR IGNORE INTO ocr_jobs (capture_id, asset_id, status, queued_at)
         VALUES (?1, ?2, 'queued', ?3)",
        params![capture_id, asset_id, at],
    )
    .map_err(err)?;
    conn.execute(
        "INSERT OR IGNORE INTO capture_ocr (capture_id, status, updated_at)
         VALUES (?1, 'queued', ?2)",
        params![capture_id, at],
    )
    .map_err(err)?;
    Ok(true)
}

pub fn claim_next_job(conn: &mut Connection) -> Result<Option<(String, String)>, String> {
    let transaction = conn.transaction().map_err(err)?;
    let job = transaction
        .query_row(
            "SELECT j.capture_id, a.path FROM ocr_jobs j
             JOIN capture_assets a ON a.id = j.asset_id
             WHERE j.status = 'queued' AND a.path IS NOT NULL
             ORDER BY j.queued_at LIMIT 1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(err)?;
    if let Some((capture_id, _)) = &job {
        let at = chrono::Utc::now().to_rfc3339();
        transaction
            .execute(
                "UPDATE ocr_jobs SET status = 'running', attempts = attempts + 1, started_at = ?2
             WHERE capture_id = ?1 AND status = 'queued'",
                params![capture_id, at],
            )
            .map_err(err)?;
        transaction
            .execute(
                "UPDATE capture_ocr SET status = 'running', error = NULL, updated_at = ?2
             WHERE capture_id = ?1",
                params![capture_id, at],
            )
            .map_err(err)?;
    }
    transaction.commit().map_err(err)?;
    Ok(job)
}

pub fn finish_job(
    conn: &mut Connection,
    capture_id: &str,
    result: Result<String, String>,
) -> Result<Vec<String>, String> {
    let transaction = conn.transaction().map_err(err)?;
    let at = chrono::Utc::now().to_rfc3339();
    match result {
        Ok(text) => {
            transaction.execute(
                "UPDATE ocr_jobs SET status = 'done', completed_at = ?2, error = NULL WHERE capture_id = ?1",
                params![capture_id, at],
            ).map_err(err)?;
            transaction.execute(
                "UPDATE capture_ocr SET status = 'done', text = ?2, error = NULL, updated_at = ?3 WHERE capture_id = ?1",
                params![capture_id, text, at],
            ).map_err(err)?;
        }
        Err(error) => {
            transaction.execute(
                "UPDATE ocr_jobs SET status = 'failed', completed_at = ?2, error = ?3 WHERE capture_id = ?1",
                params![capture_id, at, error],
            ).map_err(err)?;
            transaction.execute(
                "UPDATE capture_ocr SET status = 'failed', error = ?2, updated_at = ?3 WHERE capture_id = ?1",
                params![capture_id, error, at],
            ).map_err(err)?;
        }
    }
    let contexts = crate::context_rules::apply_enabled_rules(&transaction, capture_id)?;
    crate::search_index::refresh_search_document(&transaction, capture_id, &at).map_err(err)?;
    transaction.commit().map_err(err)?;
    Ok(contexts)
}

#[cfg(windows)]
pub fn run_local(path: &Path) -> Result<String, String> {
    use windows::{
        core::HSTRING,
        Graphics::Imaging::BitmapDecoder,
        Media::Ocr::OcrEngine,
        Storage::{FileAccessMode, StorageFile},
        Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED},
    };
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(path.display().to_string()))
        .map_err(err)?
        .get()
        .map_err(err)?;
    let stream = file
        .OpenAsync(FileAccessMode::Read)
        .map_err(err)?
        .get()
        .map_err(err)?;
    let decoder = BitmapDecoder::CreateAsync(&stream)
        .map_err(err)?
        .get()
        .map_err(err)?;
    let bitmap = decoder
        .GetSoftwareBitmapAsync()
        .map_err(err)?
        .get()
        .map_err(err)?;
    let limit = OcrEngine::MaxImageDimension().map_err(err)?;
    if bitmap.PixelWidth().map_err(err)? as u32 > limit
        || bitmap.PixelHeight().map_err(err)? as u32 > limit
    {
        return Err(format!("Imagem acima do limite local de OCR ({limit}px)."));
    }
    let engine = OcrEngine::TryCreateFromUserProfileLanguages().map_err(err)?;
    let result = engine
        .RecognizeAsync(&bitmap)
        .map_err(err)?
        .get()
        .map_err(err)?;
    Ok(result.Text().map_err(err)?.to_string())
}

#[cfg(not(windows))]
pub fn run_local(_path: &Path) -> Result<String, String> {
    Err("OCR local está disponível nesta versão para Windows.".into())
}

fn err(error: impl ToString) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{schema, storage::open_or_create};

    #[test]
    fn image_ocr_queue_recovers_and_indexes_local_result() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open_or_create(dir.path(), &"a3".repeat(32)).unwrap();
        schema::migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO captures (id, context_id, content_text, content_hash, captured_at, platform, metadata_json, capture_kind, created_at)
             VALUES ('capture', 'inbox', '', 'hash', '2026-11-15T10:00:00Z', 'windows', '{}', 'capture', 'now')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO capture_assets (id, capture_id, kind, path, status, created_at)
             VALUES ('asset', 'capture', 'clipboard_image', 'C:/assets/image.png', 'saved', 'now')",
            [],
        )
        .unwrap();

        assert!(enqueue_capture(&conn, "capture").unwrap());
        assert_eq!(enqueue_pending_jobs(&conn).unwrap(), 0);
        let mut conn = conn;
        assert_eq!(
            claim_next_job(&mut conn).unwrap(),
            Some(("capture".into(), "C:/assets/image.png".into()))
        );
        recover_interrupted_jobs(&conn).unwrap();
        assert_eq!(claim_next_job(&mut conn).unwrap().unwrap().0, "capture");
        finish_job(&mut conn, "capture", Ok("local OCR result".into())).unwrap();

        let (status, text): (String, String) = conn
            .query_row(
                "SELECT status, text FROM capture_ocr WHERE capture_id = 'capture'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "done");
        assert_eq!(text, "local OCR result");
        let indexed: String = conn
            .query_row(
                "SELECT ocr FROM capture_search_documents WHERE capture_id = 'capture'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(indexed.contains("local OCR result"));
    }
}
