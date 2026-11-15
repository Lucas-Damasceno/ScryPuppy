//! Reconstruct saved clipboard representations without a UI runtime.
use crate::{
    clipboard::{
        self, ClipboardFile, ClipboardFileAvailability, ClipboardFileKind, ClipboardImage,
        ClipboardRepresentation, ClipboardSnapshot,
    },
    library::{CaptureDto, CaptureFileDto},
};
use std::path::PathBuf;
fn err(error: impl ToString) -> String {
    error.to_string()
}

pub fn snapshot_from_capture(capture: &CaptureDto) -> Result<ClipboardSnapshot, String> {
    let mut representations = Vec::new();
    for representation in capture
        .representations
        .iter()
        .filter(|representation| representation.restorable)
    {
        match representation.kind.as_str() {
            "plain_text" => {
                if let Some(value) = &representation.text_content {
                    representations.push(ClipboardRepresentation::PlainText(value.clone()));
                }
            }
            "html" => {
                if let Some(value) = &representation.text_content {
                    representations.push(ClipboardRepresentation::Html(value.clone()));
                }
            }
            "rich_text" => {
                if let Some(value) = &representation.text_content {
                    representations.push(ClipboardRepresentation::RichText(value.clone()));
                }
            }
            "url" => {
                if let Some(value) = &representation.text_content {
                    representations.push(ClipboardRepresentation::Url(value.clone()));
                }
            }
            "image" => {
                if let Some(path) = &representation.asset_path {
                    let image = image::open(path).map_err(err)?.to_rgba8();
                    let (width, height) = image.dimensions();
                    representations.push(ClipboardRepresentation::Image(ClipboardImage {
                        width: width as usize,
                        height: height as usize,
                        rgba: image.into_raw(),
                    }));
                }
            }
            "files" => {
                let files = capture
                    .files
                    .iter()
                    .filter(|file| file.representation_id == representation.id)
                    .filter_map(clipboard_file_from_dto)
                    .collect::<Vec<_>>();
                if !files.is_empty() {
                    representations.push(ClipboardRepresentation::Files(files));
                }
            }
            _ => {}
        }
    }
    if representations.is_empty() {
        return Err("This capture has no restorable clipboard representation.".into());
    }
    Ok(ClipboardSnapshot {
        representations,
        formats: Vec::new(),
    })
}

fn clipboard_file_from_dto(file: &CaptureFileDto) -> Option<ClipboardFile> {
    let path = file
        .local_path
        .as_deref()
        .or(file.original_path.as_deref())
        .map(PathBuf::from)?;
    let kind = match file.entry_kind.as_str() {
        "directory" => ClipboardFileKind::Directory,
        "application" => ClipboardFileKind::Application,
        "shortcut" => ClipboardFileKind::Shortcut,
        "virtual_file" => ClipboardFileKind::VirtualFile,
        _ => ClipboardFileKind::File,
    };
    let availability = if clipboard::is_network_path(&path) {
        ClipboardFileAvailability::Unverified
    } else if path.exists() {
        ClipboardFileAvailability::Available
    } else {
        return None;
    };
    Some(ClipboardFile {
        display_name: file.display_name.clone(),
        original_path: Some(path),
        kind,
        size_bytes: file.size_bytes.map(|value| value.max(0) as u64),
        bytes: None,
        availability,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::CaptureRepresentationDto;
    use serde_json::json;

    fn capture() -> CaptureDto {
        serde_json::from_value(json!({
            "id":"c", "content_text":"texto", "captured_at":"2026",
            "source_app_name":null, "source_app_id":null, "source_process_id":null,
            "source_process_path":null, "window_title":null, "window_id":null,
            "platform":"windows", "kind":"capture", "content_kind":"plain_text",
            "metadata":{}, "assets":[], "representations":[], "files":[],
            "clipboard_formats":[], "tags":[], "entities":[], "ocr":null, "contexts":[]
        }))
        .unwrap()
    }

    fn text_representation(
        kind: &str,
        content: &str,
        restorable: bool,
    ) -> CaptureRepresentationDto {
        CaptureRepresentationDto {
            id: kind.into(),
            kind: kind.into(),
            format_name: kind.into(),
            mime_type: None,
            text_content: Some(content.into()),
            asset_path: None,
            size_bytes: None,
            sha256: None,
            restorable,
            metadata: json!({}),
        }
    }

    #[test]
    fn restores_all_available_text_formats_in_order() {
        let mut item = capture();
        item.representations = vec![
            text_representation("plain_text", "texto", true),
            text_representation("html", "<b>texto</b>", true),
            text_representation("rich_text", "{\\rtf1 texto}", true),
            text_representation("url", "https://example.com", true),
            text_representation("plain_text", "ignored", false),
        ];
        let restored = snapshot_from_capture(&item).unwrap();
        assert_eq!(restored.representations.len(), 4);
        assert!(
            matches!(&restored.representations[0], ClipboardRepresentation::PlainText(v) if v == "texto")
        );
        assert!(
            matches!(&restored.representations[1], ClipboardRepresentation::Html(v) if v == "<b>texto</b>")
        );
        assert!(
            matches!(&restored.representations[2], ClipboardRepresentation::RichText(v) if v.starts_with("{\\rtf1"))
        );
        assert!(
            matches!(&restored.representations[3], ClipboardRepresentation::Url(v) if v == "https://example.com")
        );
    }

    #[test]
    fn missing_file_is_skipped_but_unc_path_is_not_probed() {
        let mut item = capture();
        item.representations
            .push(text_representation("files", "", true));
        let missing = tempfile::tempdir().unwrap().path().join("missing.txt");
        item.files = vec![
            serde_json::from_value(json!({"id":"f1", "representation_id":"files", "ordinal":0,
                "display_name":"missing.txt", "original_path":missing, "local_path":null,
                "entry_kind":"file", "extension":"txt", "size_bytes":null, "sha256":null,
                "availability":"missing", "metadata":{}})).unwrap(),
            serde_json::from_value(json!({"id":"f2", "representation_id":"files", "ordinal":1,
                "display_name":"remote.txt", "original_path":"\\\\server\\share\\remote.txt", "local_path":null,
                "entry_kind":"file", "extension":"txt", "size_bytes":null, "sha256":null,
                "availability":"unverified", "metadata":{}})).unwrap(),
        ];
        let restored = snapshot_from_capture(&item).unwrap();
        match &restored.representations[0] {
            ClipboardRepresentation::Files(files) => {
                assert_eq!(files.len(), 1);
                assert_eq!(files[0].availability, ClipboardFileAvailability::Unverified);
                assert_eq!(files[0].display_name, "remote.txt");
            }
            _ => panic!("expected files"),
        }
    }

    #[test]
    fn empty_snapshot_returns_existing_error() {
        assert_eq!(
            snapshot_from_capture(&capture()).unwrap_err(),
            "This capture has no restorable clipboard representation."
        );
    }

    #[test]
    fn restores_image_pixels_from_the_saved_asset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.png");
        let pixels = image::RgbaImage::from_raw(1, 1, vec![12, 34, 56, 255]).unwrap();
        pixels.save(&path).unwrap();
        let mut item = capture();
        let mut representation = text_representation("image", "", true);
        representation.asset_path = Some(path.to_string_lossy().into_owned());
        item.representations.push(representation);
        let restored = snapshot_from_capture(&item).unwrap();
        match &restored.representations[0] {
            ClipboardRepresentation::Image(image) => {
                assert_eq!((image.width, image.height), (1, 1));
                assert_eq!(image.rgba, vec![12, 34, 56, 255]);
            }
            _ => panic!("expected image"),
        }
    }
}
