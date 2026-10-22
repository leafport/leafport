//! Decrypted publication files and cover images.
//!
//! A fulfilled ADEPT EPUB is rewritten as a plain EPUB (rights.xml dropped, ADEPT
//! resources decrypted, font obfuscation kept); an EBX PDF is decrypted with lopdf.
//! Covers come from the EPUB's OPF or from page 1 of a PDF rendered by pdfium.

use std::collections::{HashMap, HashSet};
use std::io::{self, Cursor, Read, Write};
use std::path::PathBuf;
use std::sync::OnceLock;

use pdfium_render::prelude::{PdfRenderConfig, Pdfium};
use rsa::RsaPrivateKey;
use zip::ZipArchive;
use zip::write::{SimpleFileOptions, ZipWriter};

use super::{Format, LibraryError};
use crate::drm::{XMLENC, is_adept_data};
use crate::{drm, fulfillment, pdf_drm};

const RIGHTS: &str = "META-INF/rights.xml";
const ENCRYPTION: &str = "META-INF/encryption.xml";
/// Caps for writing a decrypted EPUB.
const MAX_ENTRIES: usize = 10_000;
const MAX_OUTPUT_BYTES: usize = 512 * 1024 * 1024;
/// Maximum size of one XML entry (container, OPF, rights, encryption).
const MAX_XML_BYTES: usize = 16 * 1024 * 1024;
const MAX_COVER_BYTES: usize = 10 * 1024 * 1024;
const PDF_COVER_WIDTH: i32 = 300;

/// A cover image as stored next to the book: `cover.<extension>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cover {
    /// `"jpg"` or `"png"`.
    pub extension: &'static str,
    pub bytes: Vec<u8>,
}

fn invalid(message: impl Into<String>) -> LibraryError {
    LibraryError::Io(io::Error::new(io::ErrorKind::InvalidData, message.into()))
}

fn drm_error(error: impl std::fmt::Display) -> LibraryError {
    LibraryError::Drm(error.to_string())
}

/// Read at most `limit` bytes; `None` when the stream is larger or unreadable.
fn read_limited(reader: impl Read, limit: usize) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    reader.take(limit as u64 + 1).read_to_end(&mut out).ok()?;
    (out.len() <= limit).then_some(out)
}

fn read_text<R: Read + io::Seek>(zip: &mut ZipArchive<R>, name: &str) -> Option<String> {
    String::from_utf8(read_limited(zip.by_name(name).ok()?, MAX_XML_BYTES)?).ok()
}

fn parse_xml(text: &str) -> Option<roxmltree::Document<'_>> {
    roxmltree::Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 500_000,
            entity_resolver: None,
        },
    )
    .ok()
}

/// Turn a fulfilled download into the file written to the title folder. Unprotected
/// EPUBs and PDFs are returned unchanged. DRM failures map to `LibraryError::Drm`.
pub fn decrypt_publication(
    body: &[u8],
    format: Format,
    license_token_xml: Option<&str>,
    license_key: &RsaPrivateKey,
) -> Result<Vec<u8>, LibraryError> {
    match format {
        Format::Epub => decrypt_epub(body, license_token_xml, license_key),
        Format::Pdf => decrypt_pdf(body, license_token_xml, license_key),
        Format::Unknown => Err(LibraryError::UnsupportedFormat),
    }
}

fn decrypt_pdf(
    body: &[u8],
    license_token_xml: Option<&str>,
    license_key: &RsaPrivateKey,
) -> Result<Vec<u8>, LibraryError> {
    if !pdf_drm::is_ebx_encrypted(body) {
        return Ok(body.to_vec());
    }
    // The book key comes from the PDF's embedded ADEPT_LICENSE, else the licenseToken.
    let content_key = match pdf_drm::content_key(body, license_key) {
        Ok(key) => key,
        Err(embedded) => match license_token_xml {
            Some(token) => pdf_drm::unwrap_license_key(token, license_key).map_err(|e| {
                LibraryError::Drm(format!("embedded license: {embedded}; licenseToken: {e}"))
            })?,
            None => return Err(drm_error(embedded)),
        },
    };
    pdf_drm::decrypt_pdf(body, &content_key).map_err(drm_error)
}

fn decrypt_epub(
    body: &[u8],
    license_token_xml: Option<&str>,
    license_key: &RsaPrivateKey,
) -> Result<Vec<u8>, LibraryError> {
    let mut zip = ZipArchive::new(Cursor::new(body)).map_err(io::Error::from)?;
    let encryption = if zip.by_name(ENCRYPTION).is_ok() {
        read_text(&mut zip, ENCRYPTION).ok_or_else(|| invalid("unreadable encryption.xml"))?
    } else {
        return Ok(body.to_vec());
    };
    if !drm::is_adept_encryption(&encryption) {
        return Ok(body.to_vec());
    }
    if zip.len() > MAX_ENTRIES {
        return Err(invalid("EPUB has too many entries"));
    }
    // ADEPT servers deliver the EPUB without rights.xml; its licenseToken comes in the
    // fulfillment response, and is written into the book as rights.xml.
    let rights = read_text(&mut zip, RIGHTS)
        .or_else(|| license_token_xml.map(str::to_owned))
        .ok_or_else(|| {
            LibraryError::Drm("protected EPUB has no rights.xml or licenseToken".into())
        })?;
    let key = drm::content_key(&rights, license_key).map_err(drm_error)?;
    // Match CipherReference URIs to archive paths by percent-decoding them.
    let listed: Vec<(String, String, bool)> = drm::encrypted_resources(&encryption)
        .map_err(drm_error)?
        .into_iter()
        .map(|resource| {
            let decoded = percent_encoding::percent_decode_str(&resource.uri)
                .decode_utf8()
                .map(|s| s.into_owned())
                .unwrap_or_else(|_| resource.uri.clone());
            (decoded, resource.uri, resource.compressed)
        })
        .collect();
    let resources: HashMap<&str, bool> = listed
        .iter()
        .map(|(decoded, _, compressed)| (decoded.as_str(), *compressed))
        .collect();
    // The listed resources that were decrypted. One without an entry would leave the
    // book with ciphertext whose encryption.xml entry is stripped.
    let mut matched = HashSet::new();
    let remaining_encryption = strip_adept_entries(&encryption);

    let mut out = ZipWriter::new(Cursor::new(Vec::new()));
    let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let deflated =
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut total = 0usize;
    fn add(total: &mut usize, bytes: usize) -> Result<(), LibraryError> {
        *total = total.saturating_add(bytes);
        if *total > MAX_OUTPUT_BYTES {
            return Err(invalid("decrypted EPUB exceeds 512 MiB"));
        }
        Ok(())
    }
    if let Ok(file) = zip.by_name("mimetype") {
        let bytes = read_limited(file, 1024).ok_or_else(|| invalid("invalid mimetype entry"))?;
        out.start_file("mimetype", stored)
            .map_err(io::Error::from)?;
        out.write_all(&bytes)?;
    }
    for index in 0..zip.len() {
        let name = zip
            .by_index_raw(index)
            .map_err(io::Error::from)?
            .name()
            .map_err(io::Error::from)?
            .into_owned();
        match name.as_str() {
            "mimetype" | RIGHTS => {}
            ENCRYPTION => {
                if let Some(text) = &remaining_encryption {
                    add(&mut total, text.len())?;
                    out.start_file(ENCRYPTION, deflated)
                        .map_err(io::Error::from)?;
                    out.write_all(text.as_bytes())?;
                }
            }
            _ => match resources.get(name.as_str()) {
                Some(&compressed) => {
                    matched.insert(name.clone());
                    let file = zip.by_index(index).map_err(io::Error::from)?;
                    let raw = read_limited(file, MAX_OUTPUT_BYTES - total)
                        .ok_or_else(|| invalid(format!("{name}: entry too large")))?;
                    let plain = drm::decrypt_resource(&raw, &key, compressed)
                        .map_err(|e| LibraryError::Drm(format!("{name}: {e}")))?;
                    add(&mut total, plain.len())?;
                    out.start_file(name.as_str(), deflated)
                        .map_err(io::Error::from)?;
                    out.write_all(&plain)?;
                }
                None => {
                    let file = zip.by_index_raw(index).map_err(io::Error::from)?;
                    add(&mut total, file.compressed_size() as usize)?;
                    out.raw_copy_file(file).map_err(io::Error::from)?;
                }
            },
        }
    }
    if let Some((_, uri, _)) = listed
        .iter()
        .find(|(decoded, ..)| !matched.contains(decoded))
    {
        return Err(LibraryError::Drm(format!(
            "encrypted resource {uri} is not in the EPUB"
        )));
    }
    Ok(out.finish().map_err(io::Error::from)?.into_inner())
}

/// encryption.xml with the ADEPT `EncryptedData` elements cut out of the original text,
/// or `None` when no `EncryptedData` remains (or the XML does not parse).
fn strip_adept_entries(encryption: &str) -> Option<String> {
    let document = parse_xml(encryption)?;
    let data: Vec<_> = document
        .descendants()
        .filter(|node| node.has_tag_name((XMLENC, "EncryptedData")))
        .collect();
    let mut cut: Vec<_> = data
        .iter()
        .filter(|node| is_adept_data(**node))
        .map(|node| node.range())
        .collect();
    if cut.len() == data.len() {
        return None;
    }
    cut.sort_by_key(|range| range.start);
    let mut out = String::with_capacity(encryption.len());
    let mut position = 0;
    for range in cut {
        // EncryptedData elements may nest only in malformed input; skip overlaps.
        if range.start < position {
            continue;
        }
        out.push_str(&encryption[position..range.start]);
        position = range.end;
    }
    out.push_str(&encryption[position..]);
    Some(out)
}

/// The loan token from an EPUB's `META-INF/rights.xml`, retained verbatim as a
/// standalone element. `None` when the book is not a loan.
pub fn epub_loan_token(body: &[u8]) -> Option<String> {
    let mut zip = ZipArchive::new(Cursor::new(body)).ok()?;
    let mut rights = String::new();
    zip.by_name(RIGHTS)
        .ok()?
        .take(4 * 1024 * 1024)
        .read_to_string(&mut rights)
        .ok()?;
    // A loan is marked by a loanToken element in the rights; retain it verbatim.
    let document = roxmltree::Document::parse_with_options(
        &rights,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 50_000,
            entity_resolver: None,
        },
    )
    .ok()?;
    let node = document
        .descendants()
        .find(|node| node.has_tag_name(("http://ns.adobe.com/adept", "loanToken")))?;
    Some(fulfillment::standalone_element(&rights, node))
}

/// The cover of a decrypted publication. Any failure yields `None`.
pub fn extract_cover(publication: &[u8], format: Format) -> Option<Cover> {
    match format {
        Format::Epub => epub_cover(publication),
        Format::Pdf => pdf_cover(publication),
        Format::Unknown => None,
    }
}

/// The OPF path and text of an EPUB, found through `META-INF/container.xml`.
fn read_opf<R: Read + io::Seek>(zip: &mut ZipArchive<R>) -> Option<(String, String)> {
    let container = read_text(zip, "META-INF/container.xml")?;
    let opf_path = parse_xml(&container)?
        .descendants()
        .find(|node| node.has_tag_name("rootfile"))?
        .attribute("full-path")?
        .to_owned();
    let opf = read_text(zip, &opf_path)?;
    Some((opf_path, opf))
}

fn epub_cover(publication: &[u8]) -> Option<Cover> {
    let mut zip = ZipArchive::new(Cursor::new(publication)).ok()?;
    let (opf_path, opf) = read_opf(&mut zip)?;
    let document = parse_xml(&opf)?;
    let items: Vec<_> = document
        .descendants()
        .filter(|node| node.has_tag_name("item"))
        .collect();
    // EPUB2 names the cover item in <meta name="cover">; EPUB3 marks it cover-image.
    let cover_id = document
        .descendants()
        .find(|node| node.has_tag_name("meta") && node.attribute("name") == Some("cover"))
        .and_then(|node| node.attribute("content"));
    let item = cover_id
        .and_then(|id| items.iter().find(|item| item.attribute("id") == Some(id)))
        .or_else(|| {
            items.iter().find(|item| {
                item.attribute("properties")
                    .is_some_and(|p| p.split_whitespace().any(|p| p == "cover-image"))
            })
        })?;
    let extension = match item.attribute("media-type")? {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        _ => return None,
    };
    let path = resolve(&opf_path, item.attribute("href")?)?;
    let bytes = read_limited(zip.by_name(&path).ok()?, MAX_COVER_BYTES)?;
    Some(Cover { extension, bytes })
}

/// The title of a decrypted publication as the book itself declares it: the first
/// `dc:title` of an EPUB's OPF, or a PDF's `/EBX_TITLE` or `/Title`.
/// PDFs must be the original download: decryption removes `/Encrypt`.
/// Any failure, or an empty title, yields `None`.
pub fn book_title(publication: &[u8], format: Format) -> Option<String> {
    let raw = match format {
        Format::Epub => epub_title(publication),
        Format::Pdf => pdf_title(publication),
        Format::Unknown => None,
    }?;
    let title = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    (!title.is_empty()).then_some(title)
}

fn epub_title(publication: &[u8]) -> Option<String> {
    let mut zip = ZipArchive::new(Cursor::new(publication)).ok()?;
    let (_, opf) = read_opf(&mut zip)?;
    let document = parse_xml(&opf)?;
    let metadata = document
        .descendants()
        .find(|node| node.has_tag_name("metadata"))?;
    let title = metadata
        .descendants()
        .find(|node| node.has_tag_name((DC, "title")))?;
    Some(title.text()?.to_owned())
}

const DC: &str = "http://purl.org/dc/elements/1.1/";

/// The title comes from one dictionary chosen by the security handler: the trailer
/// `/Encrypt` dictionary for non-Standard (Adobe EBX) DRM, else the `/Info` dictionary.
/// `/EBX_TITLE` wins over `/Title`. XMP is never consulted. Needs the original bytes:
/// decrypting drops `/Encrypt`.
fn pdf_title(publication: &[u8]) -> Option<String> {
    let document = lopdf::Document::load_mem(publication).ok()?;
    let trailer = &document.trailer;
    let is_ebx = |dict: &lopdf::Dictionary| {
        dict.get(b"Filter")
            .ok()
            .and_then(|f| f.as_name().ok())
            .is_some_and(|name| name != b"Standard")
    };
    let dict = match trailer
        .get(b"Encrypt")
        .ok()
        .and_then(|e| document.dereference(e).ok())
        .and_then(|(_, e)| e.as_dict().ok())
    {
        Some(encrypt) if is_ebx(encrypt) => encrypt,
        _ => document
            .dereference(trailer.get(b"Info").ok()?)
            .ok()?
            .1
            .as_dict()
            .ok()?,
    };
    ["EBX_TITLE", "Title"].into_iter().find_map(|key| {
        match document.dereference(dict.get(key.as_bytes()).ok()?).ok()?.1 {
            lopdf::Object::String(bytes, _) => Some(decode_pdf_text(bytes)),
            _ => None,
        }
    })
}

/// A PDF text string: UTF-16 with a BOM, UTF-8 with a BOM, else Latin-1 (PDFDocEncoding).
fn decode_pdf_text(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units = rest
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]));
        char::decode_utf16(units)
            .map(|c| c.unwrap_or('\u{FFFD}'))
            .collect()
    } else if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units = rest
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]));
        char::decode_utf16(units)
            .map(|c| c.unwrap_or('\u{FFFD}'))
            .collect()
    } else if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(rest).into_owned()
    } else {
        bytes.iter().map(|&b| b as char).collect()
    }
}

/// Resolve a percent-encoded `href` against the directory of `base`. External,
/// absolute, and archive-escaping references yield `None`.
fn resolve(base: &str, href: &str) -> Option<String> {
    let href = percent_encoding::percent_decode_str(href.split('#').next().unwrap_or(href))
        .decode_utf8()
        .ok()?;
    if href.contains(':') || href.starts_with('/') || href.contains('\\') {
        return None;
    }
    let mut parts: Vec<_> = base
        .rsplit_once('/')
        .map(|(dir, _)| dir.split('/').collect())
        .unwrap_or_default();
    for part in href.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            _ => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

fn pdf_cover(publication: &[u8]) -> Option<Cover> {
    let document = pdfium()?.load_pdf_from_byte_slice(publication, None).ok()?;
    let config = PdfRenderConfig::new().set_target_width(PDF_COVER_WIDTH);
    let image = document
        .pages()
        .first()
        .ok()?
        .render_with_config(&config)
        .ok()?
        .as_image()
        .ok()?;
    let mut bytes = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
        .ok()?;
    Some(Cover {
        extension: "png",
        bytes,
    })
}

/// Whether a pdfium library could be bound, i.e. PDF covers can be rendered.
pub fn pdfium_available() -> bool {
    pdfium().is_some()
}

/// The process-wide pdfium binding, bound lazily on first use.
static PDFIUM: OnceLock<Option<Pdfium>> = OnceLock::new();

fn pdfium() -> Option<&'static Pdfium> {
    PDFIUM.get_or_init(load_pdfium).as_ref()
}

/// Locate a pdfium shared library: an explicit LEAFPORT_PDFIUM_LIB path, a file next to the
/// executable, or the system library. Returns a bound Pdfium.
fn load_pdfium() -> Option<Pdfium> {
    // 1. Explicit override.
    if let Some(path) = std::env::var_os("LEAFPORT_PDFIUM_LIB")
        && let Ok(bindings) = Pdfium::bind_to_library(PathBuf::from(path))
    {
        return Some(Pdfium::new(bindings));
    }
    // 2. A libpdfium next to the executable (how a packaged build would ship it).
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let name = Pdfium::pdfium_platform_library_name_at_path(dir);
        if let Ok(bindings) = Pdfium::bind_to_library(&name) {
            return Some(Pdfium::new(bindings));
        }
    }
    // 3. The system library.
    if let Ok(bindings) = Pdfium::bind_to_system_library() {
        return Some(Pdfium::new(bindings));
    }
    None
}
