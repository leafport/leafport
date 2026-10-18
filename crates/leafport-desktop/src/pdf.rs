//! PDF reading: page rendering via pdfium when available, text extraction otherwise.
//!
//! Adobe EBX-DRM PDFs are decrypted first (see leafport_core::pdf_drm) when the device license
//! key and the fulfillment licenseToken are available; otherwise an encrypted PDF opens
//! blocked. When a pdfium library is found, each page is rasterized to an image and shown
//! like EPUB images; without pdfium, the text layer is extracted and reflowed.

use crate::epub::{Book, Chapter, EpubImage};
use leafport_core::rsa::RsaPrivateKey;
use leafport_core::{drm, pdf_drm};
use pdfium_render::prelude::{PdfRenderConfig, Pdfium};
use std::path::{Path, PathBuf};

/// Locate a pdfium shared library: an explicit LEAFPORT_PDFIUM_LIB path, a file next to the
/// executable, the system library, or a known pypdfium2 install. Returns a bound Pdfium.
fn load_pdfium() -> Option<Pdfium> {
    // 1. Explicit override.
    if let Some(path) = std::env::var_os("LEAFPORT_PDFIUM_LIB") {
        if let Ok(bindings) = Pdfium::bind_to_library(PathBuf::from(path)) {
            return Some(Pdfium::new(bindings));
        }
    }
    // 2. A libpdfium next to the executable (how a packaged build would ship it).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let name = Pdfium::pdfium_platform_library_name_at_path(dir);
            if let Ok(bindings) = Pdfium::bind_to_library(&name) {
                return Some(Pdfium::new(bindings));
            }
        }
    }
    // 3. The system library.
    if let Ok(bindings) = Pdfium::bind_to_system_library() {
        return Some(Pdfium::new(bindings));
    }
    // 4. A pypdfium2-provided libpdfium under the user's Python site-packages.
    if let Some(home) = std::env::var_os("HOME") {
        let mut candidates: Vec<PathBuf> = Vec::new();
        let base = PathBuf::from(&home);
        for pattern in [
            ".local/lib",
        ] {
            let root = base.join(pattern);
            collect_libpdfium(&root, &mut candidates, 0);
        }
        for candidate in candidates {
            if let Ok(bindings) = Pdfium::bind_to_library(&candidate) {
                return Some(Pdfium::new(bindings));
            }
        }
    }
    None
}

/// Shallow recursive search (bounded depth) for a file named libpdfium.so under `dir`.
fn collect_libpdfium(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 6 || out.len() >= 4 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_libpdfium(&path, out, depth + 1);
        } else if path.file_name().and_then(|n| n.to_str()) == Some("libpdfium.so") {
            out.push(path);
        }
    }
}

pub fn open(
    path: &Path,
    license_key: Option<&RsaPrivateKey>,
    license_token: Option<&str>,
) -> Result<Book, String> {
    let title = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "PDF".into());
    let mut book = Book {
        path: path.to_owned(),
        title,
        author: String::new(),
        chapters: vec![],
        images: vec![],
        blocked: None,
    };
    let mut bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    // Decrypt an Adobe EBX-DRM PDF when we hold the license key and the licenseToken.
    if pdf_drm::is_ebx_encrypted(&bytes) {
        match (license_key, license_token) {
            (Some(key), Some(token)) => {
                let content_key = drm::content_key(token, key).map_err(|e| e.to_string())?;
                bytes = pdf_drm::decrypt_pdf(&bytes, &content_key).map_err(|e| e.to_string())?;
            }
            _ => {
                book.blocked = Some(
                    "This PDF is Adobe-DRM protected. Sign in with the account it was \
                     fulfilled to in order to read it. The original file is unchanged."
                        .into(),
                );
                return Ok(book);
            }
        }
    }
    // Prefer rasterized page rendering when a pdfium library is available.
    if let Some(pdfium) = load_pdfium() {
        match render_pages(&pdfium, &bytes) {
            Ok(pages) if !pages.is_empty() => {
                for (number, png) in pages.into_iter().enumerate() {
                    let index = book.images.len();
                    book.images.push(EpubImage { bytes: png });
                    book.chapters.push(Chapter {
                        title: format!("Page {}", number + 1),
                        text: String::new(),
                        href: String::new(),
                        images: vec![index],
                    });
                }
                return Ok(book);
            }
            // Fall through to text extraction when rendering yields nothing or errors.
            _ => {}
        }
    }
    extract_text(&mut book, &bytes);
    Ok(book)
}

/// Render each page to a PNG, capped in resolution and page count to bound memory.
fn render_pages(pdfium: &Pdfium, bytes: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    use std::io::Cursor;
    let document = pdfium
        .load_pdf_from_byte_slice(bytes, None)
        .map_err(|e| e.to_string())?;
    let config = PdfRenderConfig::new().set_target_width(1200);
    let mut pages = Vec::new();
    for page in document.pages().iter() {
        if pages.len() >= 2000 {
            break;
        }
        let image = page
            .render_with_config(&config)
            .map_err(|e| e.to_string())?
            .as_image()
            .map_err(|e| e.to_string())?;
        let mut png = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        pages.push(png);
    }
    Ok(pages)
}

/// Extract the text layer as a single reflowed section (fallback when pdfium is absent).
fn extract_text(book: &mut Book, bytes: &[u8]) {
    match pdf_extract::extract_text_from_mem(bytes) {
        Ok(text) => {
            let body = text
                .lines()
                .map(str::trim_end)
                .collect::<Vec<_>>()
                .join("\n");
            if body.trim().is_empty() {
                book.blocked = Some(
                    "This PDF has no extractable text and no pdfium library is available to \
                     render its pages. The original file is unchanged."
                        .into(),
                );
            } else {
                book.chapters.push(Chapter {
                    title: "Document".into(),
                    text: body,
                    images: vec![],
                    href: String::new(),
                });
            }
        }
        Err(error) => {
            book.blocked = Some(format!(
                "This PDF could not be rendered or read as text ({error}). The original file is \
                 unchanged."
            ));
        }
    }
}
