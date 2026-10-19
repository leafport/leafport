//! PDF reading: page rendering via pdfium when available, text extraction otherwise.
//!
//! Adobe EBX-DRM PDFs are decrypted first (see leafport_core::pdf_drm) when the device license
//! key is available; the book key comes from the PDF's own ADEPT_LICENSE, falling back to
//! the fulfillment licenseToken. Otherwise an encrypted PDF opens blocked. When a pdfium
//! library is found (LEAFPORT_PDFIUM_LIB, next to the executable, or the system library), each
//! page is rasterized to an image and shown like EPUB images; without pdfium, or for very
//! long documents, the text layer is extracted and reflowed.

use crate::epub::{Book, Chapter, EpubImage};
use leafport_core::rsa::RsaPrivateKey;
use leafport_core::pdf_drm;
use pdfium_render::prelude::{PdfRenderConfig, Pdfium};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Documents with more pages than this are read as text rather than rasterized, since all
/// rendered pages are held in memory at once.
const MAX_RENDERED_PAGES: usize = 500;

/// The process-wide pdfium binding, bound lazily on first use.
static PDFIUM: OnceLock<Option<Pdfium>> = OnceLock::new();

fn pdfium() -> Option<&'static Pdfium> {
    PDFIUM.get_or_init(load_pdfium).as_ref()
}

/// Locate a pdfium shared library: an explicit LEAFPORT_PDFIUM_LIB path, a file next to the
/// executable, or the system library. Returns a bound Pdfium.
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
    None
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
    // Decrypt an Adobe EBX-DRM PDF when we hold the license key: the book key comes from
    // the PDF's embedded ADEPT_LICENSE, else from the fulfillment licenseToken.
    if pdf_drm::is_ebx_encrypted(&bytes) {
        let Some(key) = license_key else {
            book.blocked = Some(
                "This PDF is Adobe-DRM protected. Sign in with the account it was \
                 fulfilled to in order to read it. The original file is unchanged."
                    .into(),
            );
            return Ok(book);
        };
        let content_key = match pdf_drm::content_key(&bytes, key) {
            Ok(content_key) => content_key,
            Err(embedded) => match license_token {
                Some(token) => pdf_drm::unwrap_license_key(token, key).map_err(|e| {
                    format!(
                        "could not unlock PDF: embedded license: {embedded}; \
                         licenseToken: {e}"
                    )
                })?,
                None => {
                    return Err(format!("could not unlock PDF: embedded license: {embedded}"))
                }
            },
        };
        bytes = pdf_drm::decrypt_pdf(&bytes, &content_key).map_err(|e| e.to_string())?;
    }
    // Prefer rasterized page rendering when a pdfium library is available.
    if let Some(pdfium) = pdfium() {
        match render_pages(pdfium, &bytes) {
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
            // Fall through to text extraction when rendering yields nothing, errors, or the
            // document is too long to rasterize.
            _ => {}
        }
    }
    extract_text(&mut book, &bytes);
    Ok(book)
}

/// Render each page to a PNG, capped in resolution to bound memory. Errors when the document
/// exceeds MAX_RENDERED_PAGES so the caller reads it as text instead of truncating it.
fn render_pages(pdfium: &Pdfium, bytes: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    use std::io::Cursor;
    let document = pdfium
        .load_pdf_from_byte_slice(bytes, None)
        .map_err(|e| e.to_string())?;
    let count = document.pages().len() as usize;
    if count > MAX_RENDERED_PAGES {
        return Err(format!("{count} pages exceeds the render limit"));
    }
    let config = PdfRenderConfig::new().set_target_width(1200);
    let mut pages = Vec::with_capacity(count);
    for page in document.pages().iter() {
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
