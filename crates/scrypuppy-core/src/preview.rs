//! Bounded, local-only primitives for rendering clipboard previews.
//!
//! Image decoding and text extraction operate on bytes. Managed asset reads are
//! bounded and must run on a worker; no function intentionally accesses the network.
use std::{
    fs,
    fs::OpenOptions,
    io::{Cursor, Read, Write},
    path::{Component, Path},
};

use image::{ImageFormat, ImageReader, Limits};
use sha2::{Digest, Sha256};

pub const MAX_INPUT_BYTES: usize = 20 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 16_384;
const MAX_DECODED_BYTES: u64 = 128 * 1024 * 1024;
const THUMBNAIL_BOUND: u32 = 512;
const TEXT_CHARACTERS: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewError {
    TooLarge,
    InvalidText,
    UnsupportedImage,
    InvalidDocument,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImagePreview {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Returns a cached preview keyed by the capture ID, preview kind, and exact
/// source bytes. A changed source automatically gets a new cache entry.
/// Cache failures are best-effort and never prevent generating the preview.
pub fn cached_preview<F>(
    cache_root: &Path,
    capture_id: &str,
    kind: &str,
    source: &[u8],
    max_output_bytes: usize,
    generate: F,
) -> Result<Vec<u8>, PreviewError>
where
    F: FnOnce() -> Result<Vec<u8>, PreviewError>,
{
    let capture_key = digest(capture_id.as_bytes());
    let mut content = Vec::with_capacity(kind.len() + source.len() + 1);
    content.extend_from_slice(kind.as_bytes());
    content.push(0);
    content.extend_from_slice(source);
    let content_key = digest(&content);
    let cache_dir = cache_root.join(&capture_key);
    let cache_file = cache_dir.join(format!("{content_key}.bin"));

    if safe_cache_directory(cache_root).is_ok()
        && safe_cache_directory(&cache_dir).is_ok()
        && safe_cache_file(&cache_file, max_output_bytes).is_ok()
    {
        if let Ok(bytes) = fs::read(&cache_file) {
            return Ok(bytes);
        }
    }

    let generated = generate()?;
    if generated.len() <= max_output_bytes
        && safe_cache_directory(cache_root).is_ok()
        && safe_cache_directory(&cache_dir).is_ok()
    {
        let _ = write_cache_atomically(&cache_file, &generated);
    }
    Ok(generated)
}

/// Removes the cached previews associated with one capture, without following
/// symlinks or reparse points.
pub fn remove_cached_previews(cache_root: &Path, capture_id: &str) -> Result<(), PreviewError> {
    let root_metadata = match fs::symlink_metadata(cache_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(PreviewError::UnsupportedImage),
    };
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(PreviewError::UnsupportedImage);
    }
    let capture_dir = cache_root.join(digest(capture_id.as_bytes()));
    let directory_metadata = match fs::symlink_metadata(&capture_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(PreviewError::UnsupportedImage),
    };
    if directory_metadata.file_type().is_symlink() || !directory_metadata.is_dir() {
        return Err(PreviewError::UnsupportedImage);
    }
    for entry in fs::read_dir(&capture_dir).map_err(|_| PreviewError::UnsupportedImage)? {
        let path = entry.map_err(|_| PreviewError::UnsupportedImage)?.path();
        let metadata = fs::symlink_metadata(&path).map_err(|_| PreviewError::UnsupportedImage)?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_file() {
            fs::remove_file(path).map_err(|_| PreviewError::UnsupportedImage)?;
        }
    }
    fs::remove_dir(capture_dir).map_err(|_| PreviewError::UnsupportedImage)
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn safe_cache_directory(path: &Path) -> Result<(), PreviewError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(PreviewError::UnsupportedImage)
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|_| PreviewError::UnsupportedImage)
        }
        Err(_) => Err(PreviewError::UnsupportedImage),
    }
}

fn safe_cache_file(path: &Path, max_bytes: usize) -> Result<(), PreviewError> {
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.len() > max_bytes as u64 =>
        {
            Err(PreviewError::UnsupportedImage)
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(PreviewError::UnsupportedImage)
        }
        Err(_) => Err(PreviewError::UnsupportedImage),
    }
}

fn write_cache_atomically(path: &Path, bytes: &[u8]) -> Result<(), PreviewError> {
    let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4().simple()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|_| PreviewError::UnsupportedImage)?;
    file.write_all(bytes)
        .map_err(|_| PreviewError::UnsupportedImage)?;
    file.flush().map_err(|_| PreviewError::UnsupportedImage)?;
    drop(file);
    fs::rename(&temp, path).map_err(|_| PreviewError::UnsupportedImage)
}

/// Read only an application-managed asset from a local, non-reparse path.
/// Original file references and UNC paths are intentionally excluded.
pub fn read_managed_asset(root: &Path, path: &Path) -> Result<Vec<u8>, PreviewError> {
    if !root.is_absolute()
        || !path.is_absolute()
        || crate::clipboard::is_network_path(root)
        || crate::clipboard::is_network_path(path)
        || !local_drive(root)
    {
        return Err(PreviewError::UnsupportedImage);
    }
    let relative = path
        .strip_prefix(root)
        .map_err(|_| PreviewError::UnsupportedImage)?;
    if relative
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(PreviewError::UnsupportedImage);
    }
    // Inspect ancestors first so a junction/symlink cannot direct a normal asset
    // lookup into a network location or another directory.
    for ancestor in root.ancestors().filter(|p| p.parent().is_some()) {
        reject_reparse(ancestor)?;
    }
    let mut checked = root.to_path_buf();
    for part in relative.components() {
        checked.push(part);
        reject_reparse(&checked)?;
    }
    let file = fs::File::open(path).map_err(|_| PreviewError::UnsupportedImage)?;
    let metadata = file
        .metadata()
        .map_err(|_| PreviewError::UnsupportedImage)?;
    if !metadata.is_file() {
        return Err(PreviewError::UnsupportedImage);
    }
    if metadata.len() > MAX_INPUT_BYTES as u64 {
        return Err(PreviewError::TooLarge);
    }
    let mut bytes = Vec::new();
    file.take(MAX_INPUT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| PreviewError::UnsupportedImage)?;
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(PreviewError::TooLarge);
    }
    Ok(bytes)
}

/// Read a bounded local file reference for an on-device preview. Network paths,
/// reparse points, relative paths, and non-local drives are rejected.
pub fn read_local_file_preview(path: &Path) -> Result<Vec<u8>, PreviewError> {
    if !path.is_absolute()
        || crate::clipboard::is_network_path(path)
        || !local_drive(path)
        || path
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
    {
        return Err(PreviewError::UnsupportedImage);
    }
    for ancestor in path.ancestors().filter(|part| part.parent().is_some()) {
        reject_reparse(ancestor)?;
    }
    let file = fs::File::open(path).map_err(|_| PreviewError::UnsupportedImage)?;
    let metadata = file
        .metadata()
        .map_err(|_| PreviewError::UnsupportedImage)?;
    if !metadata.is_file() {
        return Err(PreviewError::UnsupportedImage);
    }
    if metadata.len() > MAX_INPUT_BYTES as u64 {
        return Err(PreviewError::TooLarge);
    }
    let mut bytes = Vec::new();
    file.take(MAX_INPUT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| PreviewError::UnsupportedImage)?;
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(PreviewError::TooLarge);
    }
    Ok(bytes)
}

fn reject_reparse(path: &Path) -> Result<(), PreviewError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| PreviewError::UnsupportedImage)?;
    if metadata.file_type().is_symlink() {
        return Err(PreviewError::UnsupportedImage);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(PreviewError::UnsupportedImage);
        }
    }
    Ok(())
}

#[cfg(windows)]
fn local_drive(path: &Path) -> bool {
    use std::{os::windows::ffi::OsStrExt, path::Prefix};
    use windows::{core::PCWSTR, Win32::Storage::FileSystem::GetDriveTypeW};
    let Some(Component::Prefix(prefix)) = path.components().next() else {
        return false;
    };
    let drive = match prefix.kind() {
        Prefix::Disk(d) | Prefix::VerbatimDisk(d) => d,
        _ => return false,
    };
    let root = std::ffi::OsString::from(format!("{}:\\", drive as char));
    let wide: Vec<u16> = root.encode_wide().chain(Some(0)).collect();
    // Unknown, missing, and remote drives fail closed.
    !matches!(unsafe { GetDriveTypeW(PCWSTR(wide.as_ptr())) }, 0 | 1 | 4)
}

#[cfg(not(windows))]
fn local_drive(_: &Path) -> bool {
    true
}

/// Generate a UTF-8 text excerpt without splitting a character.
pub fn text_excerpt(bytes: &[u8]) -> Result<String, PreviewError> {
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(PreviewError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| PreviewError::InvalidText)?;
    Ok(text
        .trim_start_matches('\u{feff}')
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
        .take(TEXT_CHARACTERS)
        .collect())
}

/// Extract a short local text preview from the main WordprocessingML document
/// in a DOCX archive. Both the archive and its expanded document part are bounded.
pub fn docx_excerpt(bytes: &[u8]) -> Result<String, PreviewError> {
    const MAX_DOCUMENT_XML_BYTES: usize = 2 * 1024 * 1024;
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(PreviewError::TooLarge);
    }
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| PreviewError::InvalidDocument)?;
    let document = archive
        .by_name("word/document.xml")
        .map_err(|_| PreviewError::InvalidDocument)?;
    if document.size() > MAX_DOCUMENT_XML_BYTES as u64 {
        return Err(PreviewError::TooLarge);
    }
    let mut xml = Vec::with_capacity(document.size() as usize);
    document
        .take(MAX_DOCUMENT_XML_BYTES as u64 + 1)
        .read_to_end(&mut xml)
        .map_err(|_| PreviewError::InvalidDocument)?;
    if xml.len() > MAX_DOCUMENT_XML_BYTES {
        return Err(PreviewError::TooLarge);
    }

    let mut reader = quick_xml::Reader::from_reader(xml.as_slice());
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut excerpt = String::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(quick_xml::events::Event::Text(text)) => {
                let decoded = text.decode().map_err(|_| PreviewError::InvalidDocument)?;
                excerpt.push_str(&decoded);
            }
            Ok(quick_xml::events::Event::GeneralRef(reference)) => {
                if let Some(character) = reference
                    .resolve_char_ref()
                    .map_err(|_| PreviewError::InvalidDocument)?
                {
                    excerpt.push(character);
                } else {
                    let entity = reference
                        .decode()
                        .map_err(|_| PreviewError::InvalidDocument)?;
                    if let Some(replacement) = quick_xml::escape::resolve_predefined_entity(&entity)
                    {
                        excerpt.push_str(replacement);
                    } else {
                        return Err(PreviewError::InvalidDocument);
                    }
                }
            }
            Ok(quick_xml::events::Event::CData(text)) => {
                let decoded = text.decode().map_err(|_| PreviewError::InvalidDocument)?;
                excerpt.push_str(&decoded);
            }
            Ok(quick_xml::events::Event::End(tag)) => {
                let name = tag.name();
                let local = name
                    .as_ref()
                    .rsplit(|byte| *byte == b':')
                    .next()
                    .unwrap_or_default();
                if local == b"p" && !excerpt.ends_with('\n') {
                    excerpt.push('\n');
                }
            }
            Ok(quick_xml::events::Event::Empty(tag)) => {
                let name = tag.name();
                let local = name
                    .as_ref()
                    .rsplit(|byte| *byte == b':')
                    .next()
                    .unwrap_or_default();
                if local == b"tab" {
                    excerpt.push('\t');
                } else if local == b"br" {
                    excerpt.push('\n');
                }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Err(_) => return Err(PreviewError::InvalidDocument),
            _ => {}
        }
        if excerpt.chars().count() > 1024 {
            break;
        }
        buffer.clear();
    }
    Ok(excerpt
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\r' | '\t'))
        .take(TEXT_CHARACTERS)
        .collect())
}

/// Decode an image with strict allocation/dimension bounds and emit a PNG thumbnail.
pub fn image_thumbnail(bytes: &[u8]) -> Result<ImagePreview, PreviewError> {
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(PreviewError::TooLarge);
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| PreviewError::UnsupportedImage)?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODED_BYTES);
    reader.limits(limits);
    let original = reader
        .decode()
        .map_err(|_| PreviewError::UnsupportedImage)?;
    let image = if original.width() <= THUMBNAIL_BOUND && original.height() <= THUMBNAIL_BOUND {
        original
    } else {
        original.thumbnail(THUMBNAIL_BOUND, THUMBNAIL_BOUND)
    };
    let (width, height) = (image.width(), image.height());
    let mut output = Cursor::new(Vec::new());
    image
        .write_to(&mut output, ImageFormat::Png)
        .map_err(|_| PreviewError::UnsupportedImage)?;
    Ok(ImagePreview {
        png: output.into_inner(),
        width,
        height,
    })
}

/// Render the first DOCX page locally with a bounded OOXML package expansion.
/// This renderer is used by the Windows app and does not contact a service.
#[cfg(windows)]
pub fn docx_first_page_preview(bytes: &[u8]) -> Result<Vec<u8>, PreviewError> {
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(PreviewError::TooLarge);
    }
    let document = rdocx::Document::from_bytes_with_limits(
        bytes,
        rdocx::PackageReadLimits {
            max_entries: 2048,
            max_part_uncompressed_bytes: 8 * 1024 * 1024,
            max_total_uncompressed_bytes: 32 * 1024 * 1024,
        },
    )
    .map_err(|_| PreviewError::InvalidDocument)?;
    let png = document
        .render_page_to_png_deterministic(0, 72.0)
        .map_err(|_| PreviewError::InvalidDocument)?
        .ok_or(PreviewError::InvalidDocument)?;
    if png.len() > 12 * 1024 * 1024 {
        return Err(PreviewError::TooLarge);
    }
    Ok(png)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, RgbaImage};

    #[test]
    fn text_is_bounded_and_unicode_safe() {
        let input = format!("\u{feff}{}\0end", "á".repeat(600));
        let output = text_excerpt(input.as_bytes()).unwrap();
        assert_eq!(output.chars().count(), 512);
        assert!(output.chars().all(|c| c == 'á'));
        assert_eq!(text_excerpt(&[0xff]), Err(PreviewError::InvalidText));
    }

    #[test]
    fn docx_preview_extracts_bounded_paragraph_text_and_unescapes_xml() {
        use zip::{write::SimpleFileOptions, ZipWriter};

        let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
        archive
            .start_file("word/document.xml", SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(
            &mut archive,
            r#"<w:document xmlns:w="urn:w"><w:body><w:p><w:r><w:t>Olá &amp; bem-vinda</w:t></w:r></w:p><w:p><w:r><w:tab/><w:t>segunda linha</w:t></w:r></w:p></w:body></w:document>"#
                .as_bytes(),
        )
        .unwrap();
        let bytes = archive.finish().unwrap().into_inner();
        assert_eq!(
            docx_excerpt(&bytes).unwrap(),
            "Olá & bem-vinda\n\tsegunda linha\n"
        );
        assert_eq!(
            docx_excerpt(b"not a docx"),
            Err(PreviewError::InvalidDocument)
        );
    }

    #[cfg(windows)]
    #[test]
    fn docx_first_page_preview_renders_a_real_word_page() {
        let mut document = rdocx::Document::new();
        document.add_paragraph("Prévia local da primeira página do documento.");
        let bytes = document.to_bytes().unwrap();
        let png = docx_first_page_preview(&bytes).unwrap();
        assert_eq!(image::guess_format(&png).unwrap(), ImageFormat::Png);
        let rendered = ImageReader::new(Cursor::new(png))
            .with_guessed_format()
            .unwrap()
            .decode()
            .unwrap();
        assert!(rendered.width() > 400);
        assert!(rendered.height() > 500);
    }

    #[cfg(windows)]
    #[test]
    fn docx_first_page_preview_rejects_invalid_and_oversized_inputs() {
        assert!(matches!(
            docx_first_page_preview(b"not a Word document"),
            Err(PreviewError::InvalidDocument)
        ));

        let oversized = vec![0; MAX_INPUT_BYTES + 1];
        assert!(matches!(
            docx_first_page_preview(&oversized),
            Err(PreviewError::TooLarge)
        ));
    }

    #[test]
    fn large_and_malformed_inputs_are_rejected() {
        let input = vec![0; MAX_INPUT_BYTES + 1];
        assert_eq!(text_excerpt(&input), Err(PreviewError::TooLarge));
        assert_eq!(image_thumbnail(&input), Err(PreviewError::TooLarge));
        assert_eq!(
            image_thumbnail(b"not an image"),
            Err(PreviewError::UnsupportedImage)
        );
    }

    #[test]
    fn image_preview_preserves_aspect_ratio_and_is_png() {
        let original = DynamicImage::ImageRgba8(RgbaImage::new(1024, 256));
        let mut bytes = Cursor::new(Vec::new());
        original.write_to(&mut bytes, ImageFormat::Png).unwrap();
        let preview = image_thumbnail(bytes.get_ref()).unwrap();
        assert_eq!((preview.width, preview.height), (512, 128));
        assert_eq!(image::guess_format(&preview.png).unwrap(), ImageFormat::Png);
    }

    #[test]
    fn ico_favicon_is_converted_to_png() {
        let original = DynamicImage::ImageRgba8(RgbaImage::new(32, 32));
        let mut bytes = Cursor::new(Vec::new());
        original.write_to(&mut bytes, ImageFormat::Ico).unwrap();
        let preview = image_thumbnail(bytes.get_ref()).unwrap();
        assert_eq!((preview.width, preview.height), (32, 32));
        assert_eq!(image::guess_format(&preview.png).unwrap(), ImageFormat::Png);
    }

    #[test]
    fn preview_cache_reuses_matching_source_and_invalidates_changed_content() {
        use std::cell::Cell;

        let dir = tempfile::tempdir().unwrap();
        let generated = Cell::new(0);
        let first = cached_preview(
            dir.path(),
            "capture-1",
            "image/png",
            b"source-v1",
            1024,
            || {
                generated.set(generated.get() + 1);
                Ok(b"preview-v1".to_vec())
            },
        )
        .unwrap();
        assert_eq!(first, b"preview-v1");
        let cached = cached_preview(
            dir.path(),
            "capture-1",
            "image/png",
            b"source-v1",
            1024,
            || {
                generated.set(generated.get() + 1);
                Ok(b"unused".to_vec())
            },
        )
        .unwrap();
        assert_eq!(cached, b"preview-v1");
        assert_eq!(generated.get(), 1);

        let updated = cached_preview(
            dir.path(),
            "capture-1",
            "image/png",
            b"source-v2",
            1024,
            || {
                generated.set(generated.get() + 1);
                Ok(b"preview-v2".to_vec())
            },
        )
        .unwrap();
        assert_eq!(updated, b"preview-v2");
        assert_eq!(generated.get(), 2);

        remove_cached_previews(dir.path(), "capture-1").unwrap();
        let regenerated = cached_preview(
            dir.path(),
            "capture-1",
            "image/png",
            b"source-v1",
            1024,
            || {
                generated.set(generated.get() + 1);
                Ok(b"preview-v1".to_vec())
            },
        )
        .unwrap();
        assert_eq!(regenerated, b"preview-v1");
        assert_eq!(generated.get(), 3);
    }

    #[test]
    fn only_reads_bounded_files_within_the_managed_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("assets");
        fs::create_dir(&root).unwrap();
        let path = root.join("saved.png");
        fs::write(&path, b"example").unwrap();
        assert_eq!(read_managed_asset(&root, &path).unwrap(), b"example");
        assert_eq!(read_local_file_preview(&path).unwrap(), b"example");
        assert!(read_local_file_preview(Path::new(r"\\server\share\document.docx")).is_err());
        let outside = dir.path().join("outside.png");
        fs::write(&outside, b"secret").unwrap();
        assert!(read_managed_asset(&root, &outside).is_err());
        assert!(read_managed_asset(&root, &root.join("..").join("outside.png")).is_err());
        let huge = fs::File::create(root.join("huge.png")).unwrap();
        huge.set_len(MAX_INPUT_BYTES as u64 + 1).unwrap();
        assert_eq!(
            read_managed_asset(&root, &root.join("huge.png")),
            Err(PreviewError::TooLarge)
        );
    }
}
