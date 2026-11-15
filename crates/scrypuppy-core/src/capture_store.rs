//! Persist clipboard captures with the database and managed asset layout used by
//! the existing desktop app.
use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

use chrono::{Duration, Utc};
use rusqlite::{params, Connection};
use serde_json::json;
use uuid::Uuid;

use crate::{
    clipboard::{
        self, ClipboardFile, ClipboardFileAvailability, ClipboardFileKind, ClipboardRepresentation,
        ClipboardSnapshot,
    },
    crypto::sha256_hex,
    search_index,
};

fn err(error: impl ToString) -> String {
    error.to_string()
}

struct AssetCleanup {
    image: PathBuf,
    files: PathBuf,
    committed: bool,
}

impl Drop for AssetCleanup {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.image);
            let _ = fs::remove_dir_all(&self.files);
        }
    }
}

/// Save a clipboard snapshot to an already-open ScryPuppy database.
/// Images and virtual files stay under the application's managed asset root.
pub fn save_clipboard_capture(
    conn: &mut Connection,
    app_dir: &Path,
    snapshot: &ClipboardSnapshot,
    capture_kind: &str,
) -> Result<String, String> {
    save_clipboard_capture_with_origin(conn, app_dir, snapshot, capture_kind, "explicit_hotkey")
}

pub fn save_clipboard_capture_with_origin(
    conn: &mut Connection,
    app_dir: &Path,
    snapshot: &ClipboardSnapshot,
    capture_kind: &str,
    capture_origin: &str,
) -> Result<String, String> {
    save_clipboard_capture_with_window_metadata(
        conn,
        app_dir,
        snapshot,
        capture_kind,
        capture_origin,
        None,
        None,
        None,
        None,
        None,
        None,
    )
}

/// Save a capture with the active-window provenance sampled by the desktop shell.
/// Window metadata is optional and remains local to the encrypted library.
pub fn save_clipboard_capture_with_window_metadata(
    conn: &mut Connection,
    app_dir: &Path,
    snapshot: &ClipboardSnapshot,
    capture_kind: &str,
    capture_origin: &str,
    source_app_name: Option<&str>,
    source_app_id: Option<&str>,
    source_process_id: Option<i64>,
    source_process_path: Option<&str>,
    window_title: Option<&str>,
    window_id: Option<&str>,
) -> Result<String, String> {
    let content_text = snapshot.content_text();
    if snapshot.is_empty()
        || (content_text.trim().is_empty()
            && snapshot.image().is_none()
            && snapshot.files().is_none())
    {
        return Err("O clipboard nao retornou conteudo para salvar.".into());
    }
    let now = Utc::now();
    let captured_at = now.to_rfc3339();
    let threshold = (now - Duration::seconds(2)).to_rfc3339();
    let hash = snapshot.content_hash();
    let duplicate = conn
        .query_row(
            "SELECT EXISTS(
            SELECT 1 FROM captures WHERE content_hash = ? AND captured_at >= ?
              AND context_id = 'inbox' AND capture_kind = ?
        )",
            params![hash, threshold, capture_kind],
            |row| row.get::<_, i64>(0),
        )
        .map_err(err)?
        != 0;
    if duplicate {
        return Err("DUPLICATE_CAPTURE_IGNORED".into());
    }

    let capture_id = Uuid::new_v4().to_string();
    let mut cleanup = AssetCleanup {
        image: app_dir
            .join("assets")
            .join("clipboard-images")
            .join(format!("{capture_id}.png")),
        files: app_dir
            .join("assets")
            .join("clipboard-files")
            .join(&capture_id),
        committed: false,
    };
    let metadata = json!({
        "capture_origin": capture_origin,
        "capture_source": "iced",
        "image_dimensions": snapshot.image_dimensions(),
        "active_window": {
            "app_name": source_app_name,
            "app_id": source_app_id,
            "process_id": source_process_id,
            "process_path": source_process_path,
            "title": window_title,
            "window_id": window_id,
        },
    });
    let transaction = conn.transaction().map_err(err)?;
    transaction
        .execute(
            "INSERT INTO captures (
            id, context_id, content_text, content_hash, captured_at,
            source_app_name, source_app_id, source_process_id, source_process_path,
            window_title, window_id, platform, metadata_json, capture_kind, created_at
        ) VALUES (?, 'inbox', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                capture_id,
                content_text,
                hash,
                captured_at,
                source_app_name,
                source_app_id,
                source_process_id,
                source_process_path,
                window_title,
                window_id,
                std::env::consts::OS,
                metadata.to_string(),
                capture_kind,
                captured_at
            ],
        )
        .map_err(err)?;
    if let Some(image) = snapshot.image() {
        persist_image(&transaction, app_dir, &capture_id, image, &captured_at)?;
    }
    persist_representations(&transaction, app_dir, &capture_id, snapshot, &captured_at)?;
    search_index::refresh_search_document(&transaction, &capture_id, &captured_at)?;
    transaction.commit().map_err(err)?;
    cleanup.committed = true;
    Ok(capture_id)
}

fn persist_image(
    conn: &Connection,
    app_dir: &Path,
    capture_id: &str,
    image: &crate::clipboard::ClipboardImage,
    created_at: &str,
) -> Result<(), String> {
    let expected_bytes = image
        .width
        .checked_mul(image.height)
        .and_then(|pixels| pixels.checked_mul(4));
    if image.width == 0
        || image.height == 0
        || image.width > 16_384
        || image.height > 16_384
        || expected_bytes != Some(image.rgba.len())
        || image.rgba.len() > 128 * 1024 * 1024
    {
        return Err("Dados de imagem do clipboard invalidos.".into());
    }
    let directory = app_dir.join("assets").join("clipboard-images");
    fs::create_dir_all(&directory).map_err(err)?;
    let path = directory.join(format!("{capture_id}.png"));
    let buffer = image::ImageBuffer::<image::Rgba<u8>, Vec<u8>>::from_raw(
        image.width as u32,
        image.height as u32,
        image.rgba.clone(),
    )
    .ok_or_else(|| "Dados de imagem do clipboard invalidos.".to_string())?;
    buffer.save(&path).map_err(err)?;
    conn.execute(
        "INSERT INTO capture_assets (id, capture_id, kind, path, status, error, created_at)
         VALUES (?, ?, 'clipboard_image', ?, 'saved', NULL, ?)",
        params![
            Uuid::new_v4().to_string(),
            capture_id,
            path.display().to_string(),
            created_at
        ],
    )
    .map_err(err)?;
    Ok(())
}

fn persist_representations(
    conn: &Connection,
    app_dir: &Path,
    capture_id: &str,
    snapshot: &ClipboardSnapshot,
    created_at: &str,
) -> Result<(), String> {
    for format in &snapshot.formats {
        conn.execute(
            "INSERT INTO capture_clipboard_formats (capture_id, format_id, format_name, supported) VALUES (?, ?, ?, ?)",
            params![capture_id, format.id as i64, format.name, format.supported as i64],
        ).map_err(err)?;
    }
    for (ordinal, representation) in snapshot.representations.iter().enumerate() {
        let representation_id = Uuid::new_v4().to_string();
        let (kind, format, mime, text, asset_path, size, hash, restorable, metadata) =
            match representation {
                ClipboardRepresentation::PlainText(value) => (
                    "plain_text",
                    "CF_UNICODETEXT",
                    Some("text/plain"),
                    Some(value.clone()),
                    None,
                    Some(value.len() as i64),
                    Some(sha256_hex(value.as_bytes())),
                    true,
                    json!({}),
                ),
                ClipboardRepresentation::Html(value) => (
                    "html",
                    "HTML Format",
                    Some("text/html"),
                    Some(value.clone()),
                    None,
                    Some(value.len() as i64),
                    Some(sha256_hex(value.as_bytes())),
                    true,
                    json!({"sanitization":"required_before_render"}),
                ),
                ClipboardRepresentation::RichText(value) => (
                    "rich_text",
                    "Rich Text Format",
                    Some("text/rtf"),
                    Some(value.clone()),
                    None,
                    Some(value.len() as i64),
                    Some(sha256_hex(value.as_bytes())),
                    true,
                    json!({}),
                ),
                ClipboardRepresentation::Url(value) => (
                    "url",
                    "UniformResourceLocatorW",
                    Some("text/uri-list"),
                    Some(value.clone()),
                    None,
                    Some(value.len() as i64),
                    Some(sha256_hex(value.as_bytes())),
                    true,
                    json!({}),
                ),
                ClipboardRepresentation::Image(image) => {
                    let path = app_dir
                        .join("assets")
                        .join("clipboard-images")
                        .join(format!("{capture_id}.png"));
                    (
                        "image",
                        "CF_DIBV5",
                        Some("image/png"),
                        None,
                        Some(path.display().to_string()),
                        Some(image.rgba.len() as i64),
                        Some(sha256_hex(&image.rgba)),
                        true,
                        json!({"width":image.width,"height":image.height}),
                    )
                }
                ClipboardRepresentation::Files(files) => (
                    "files",
                    if files.iter().any(|file| file.bytes.is_some()) {
                        "FileGroupDescriptorW"
                    } else {
                        "CF_HDROP"
                    },
                    Some("application/x-scrypuppy-file-list"),
                    None,
                    None,
                    Some(
                        files
                            .iter()
                            .filter_map(|file| file.size_bytes)
                            .fold(0u64, u64::saturating_add)
                            .min(i64::MAX as u64) as i64,
                    ),
                    None,
                    files.iter().any(|file| {
                        file.original_path.is_some()
                            || file.bytes.is_some()
                            || file.kind == ClipboardFileKind::Directory
                    }),
                    json!({"item_count":files.len()}),
                ),
            };
        conn.execute(
            "INSERT INTO capture_representations
             (id, capture_id, ordinal, kind, format_name, mime_type, text_content,
              asset_path, size_bytes, sha256, restorable, metadata_json, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                representation_id,
                capture_id,
                ordinal as i64,
                kind,
                format,
                mime,
                text,
                asset_path,
                size,
                hash,
                restorable as i64,
                metadata.to_string(),
                created_at
            ],
        )
        .map_err(err)?;
        if let ClipboardRepresentation::Files(files) = representation {
            persist_files(
                conn,
                app_dir,
                capture_id,
                &representation_id,
                files,
                created_at,
            )?;
        }
    }
    Ok(())
}

fn persist_files(
    conn: &Connection,
    app_dir: &Path,
    capture_id: &str,
    representation_id: &str,
    files: &[ClipboardFile],
    created_at: &str,
) -> Result<(), String> {
    let root = app_dir
        .join("assets")
        .join("clipboard-files")
        .join(capture_id);
    for (ordinal, file) in files.iter().enumerate() {
        let mut local_path: Option<PathBuf> = None;
        let mut availability = file.availability.clone();
        let mut hash = None;
        if file.original_path.is_none() {
            let name = clipboard::sanitize_file_name(&file.display_name);
            let path = root.join(format!("{ordinal:04}-{name}"));
            if file.kind == ClipboardFileKind::Directory {
                fs::create_dir_all(&path).map_err(err)?;
                local_path = Some(path);
                availability = ClipboardFileAvailability::Available;
            } else if let Some(bytes) = &file.bytes {
                fs::create_dir_all(&root).map_err(err)?;
                let temporary = root.join(format!(".{ordinal}.{}.part", Uuid::new_v4()));
                let mut output = File::create(&temporary).map_err(err)?;
                output.write_all(bytes).map_err(err)?;
                output.sync_all().map_err(err)?;
                fs::rename(&temporary, &path).map_err(err)?;
                local_path = Some(path);
                availability = ClipboardFileAvailability::Available;
                hash = Some(sha256_hex(bytes));
            }
        }
        let extension = Path::new(&file.display_name)
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase);
        conn.execute(
            "INSERT INTO capture_file_entries
             (id, capture_id, representation_id, ordinal, display_name, original_path,
              local_path, entry_kind, extension, size_bytes, sha256, availability,
              metadata_json, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, '{}', ?)",
            params![
                Uuid::new_v4().to_string(),
                capture_id,
                representation_id,
                ordinal as i64,
                file.display_name,
                file.original_path.as_ref().map(|p| p.display().to_string()),
                local_path.as_ref().map(|p| p.display().to_string()),
                file.kind.as_str(),
                extension,
                file.size_bytes.map(|v| v.min(i64::MAX as u64) as i64),
                hash,
                availability.as_str(),
                created_at
            ],
        )
        .map_err(err)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        clipboard::{ClipboardImage, ClipboardRepresentation},
        library::get_capture_by_id,
    };

    fn schema(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE captures(id TEXT, context_id TEXT, content_text TEXT, content_hash TEXT, captured_at TEXT,
                source_app_name TEXT, source_app_id TEXT, source_process_id INTEGER, source_process_path TEXT,
                window_title TEXT, window_id TEXT, platform TEXT, metadata_json TEXT, capture_kind TEXT, created_at TEXT);
            CREATE TABLE capture_assets(id TEXT, capture_id TEXT, kind TEXT, path TEXT, status TEXT, error TEXT, created_at TEXT);
            CREATE TABLE capture_representations(id TEXT, capture_id TEXT, ordinal INTEGER, kind TEXT, format_name TEXT,
                mime_type TEXT, text_content TEXT, asset_path TEXT, size_bytes INTEGER, sha256 TEXT, restorable INTEGER,
                metadata_json TEXT, created_at TEXT);
            CREATE TABLE capture_file_entries(id TEXT, capture_id TEXT, representation_id TEXT, ordinal INTEGER,
                display_name TEXT, original_path TEXT, local_path TEXT, entry_kind TEXT, extension TEXT, size_bytes INTEGER,
                sha256 TEXT, availability TEXT, metadata_json TEXT, created_at TEXT);
            CREATE TABLE capture_clipboard_formats(capture_id TEXT, format_id INTEGER, format_name TEXT, supported INTEGER);
            CREATE TABLE capture_tags(capture_id TEXT, tag TEXT);
            CREATE TABLE capture_entities(capture_id TEXT, id TEXT, kind TEXT, value TEXT, source TEXT, confidence REAL);
            CREATE TABLE capture_ocr(capture_id TEXT, status TEXT, text TEXT, error TEXT, updated_at TEXT);
            CREATE TABLE contexts(id TEXT, name TEXT, normalized_name TEXT, slug TEXT, created_at TEXT, updated_at TEXT);
            CREATE TABLE capture_contexts(capture_id TEXT, context_id TEXT, assignment_origin TEXT, confidence REAL, created_at TEXT);
            CREATE TABLE capture_search_documents(capture_id TEXT UNIQUE, content TEXT, ocr TEXT, metadata TEXT, content_hash TEXT, updated_at TEXT);
            CREATE TABLE capture_embeddings(capture_id TEXT, content_hash TEXT);
            INSERT INTO contexts VALUES ('inbox','Inbox','inbox','inbox','now','now');",
        ).unwrap();
    }

    #[test]
    fn saves_multiformat_clipboard_into_existing_schema_and_deduplicates() {
        let mut conn = Connection::open_in_memory().unwrap();
        schema(&conn);
        let dir = tempfile::tempdir().unwrap();
        let snapshot = ClipboardSnapshot {
            representations: vec![
                ClipboardRepresentation::PlainText("hello clipboard".into()),
                ClipboardRepresentation::Html("<b>hello</b>".into()),
                ClipboardRepresentation::Image(ClipboardImage {
                    width: 1,
                    height: 1,
                    rgba: vec![1, 2, 3, 255],
                }),
                ClipboardRepresentation::Files(vec![ClipboardFile {
                    display_name: "report.txt".into(),
                    original_path: None,
                    kind: ClipboardFileKind::File,
                    size_bytes: Some(4),
                    bytes: Some(b"data".to_vec()),
                    availability: ClipboardFileAvailability::Available,
                }]),
            ],
            formats: vec![],
        };
        let id = save_clipboard_capture_with_window_metadata(
            &mut conn,
            dir.path(),
            &snapshot,
            "capture",
            "explicit_hotkey",
            Some("Editor"),
            Some("editor.exe"),
            Some(123),
            Some("C:/Editor/editor.exe"),
            Some("Notes — Draft"),
            Some("window-123"),
        )
        .unwrap();
        let capture = get_capture_by_id(&conn, &id).unwrap();
        assert_eq!(capture.metadata["capture_origin"], "explicit_hotkey");
        assert_eq!(capture.source_app_name.as_deref(), Some("Editor"));
        assert_eq!(capture.source_app_id.as_deref(), Some("editor.exe"));
        assert_eq!(capture.source_process_id, Some(123));
        assert_eq!(capture.window_title.as_deref(), Some("Notes — Draft"));
        assert_eq!(capture.content_text, "report.txt");
        assert_eq!(capture.representations.len(), 4);
        assert_eq!(
            capture.representations[1].text_content.as_deref(),
            Some("<b>hello</b>")
        );
        assert!(capture
            .representations
            .iter()
            .any(|item| item.kind == "image"
                && item
                    .asset_path
                    .as_ref()
                    .is_some_and(|path| Path::new(path).is_file())));
        assert_eq!(capture.assets.len(), 1);
        assert_eq!(capture.files.len(), 1);
        let materialized = capture.files[0].local_path.as_deref().unwrap();
        assert_eq!(fs::read(materialized).unwrap(), b"data");
        assert!(save_clipboard_capture(&mut conn, dir.path(), &snapshot, "capture").is_err());
        assert_eq!(
            conn.query_row("SELECT count(*) FROM captures", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn database_failure_rolls_back_capture_and_removes_new_assets() {
        let mut conn = Connection::open_in_memory().unwrap();
        schema(&conn);
        conn.execute_batch("DROP TABLE capture_search_documents;")
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let snapshot = ClipboardSnapshot {
            representations: vec![ClipboardRepresentation::Image(ClipboardImage {
                width: 1,
                height: 1,
                rgba: vec![1, 2, 3, 255],
            })],
            formats: vec![],
        };
        assert!(save_clipboard_capture(&mut conn, dir.path(), &snapshot, "capture").is_err());
        assert_eq!(
            conn.query_row("SELECT count(*) FROM captures", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let image_dir = dir.path().join("assets/clipboard-images");
        assert_eq!(fs::read_dir(image_dir).unwrap().count(), 0);
    }
}
