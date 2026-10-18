//! Text-only EPUB reading. ZIP entries are read in memory, never extracted.
//! ADEPT-protected resources are decrypted in memory when the device license key is
//! available; the original file is never modified.
use leafport_core::drm;
use leafport_core::rsa::RsaPrivateKey;
use roxmltree::{Document, Node};
use std::{
    collections::HashMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};
use zip::ZipArchive;

/// Decryption context for one protected book: the unwrapped content key and the set of
/// encrypted archive paths with their compression flag.
struct Drm {
    key: [u8; 16],
    resources: HashMap<String, bool>,
}

/// Read one ZIP entry as text, decrypting it first when it is an ADEPT-encrypted
/// resource and a key is available. Non-encrypted entries read as before.
fn read_doc(zip: &mut ZipArchive<File>, name: &str, drm: Option<&Drm>) -> Result<String, String> {
    if let Some(drm) = drm {
        if let Some(&compressed) = drm.resources.get(name) {
            let mut raw = Vec::new();
            {
                let mut f = zip.by_name(name).map_err(|e| format!("{name}: {e}"))?;
                if f.size() > 64 * 1024 * 1024 {
                    return Err(format!("{name}: encrypted entry exceeds 64 MiB"));
                }
                f.read_to_end(&mut raw)
                    .map_err(|e| format!("{name}: {e}"))?;
            }
            let plain = drm::decrypt_resource(&raw, &drm.key, compressed)
                .map_err(|e| format!("{name}: {e}"))?;
            return String::from_utf8(plain)
                .map_err(|_| format!("{name}: decrypted resource is not UTF-8"));
        }
    }
    entry(zip, name)
}

pub struct Chapter {
    pub title: String,
    pub text: String,
    /// Indices into `Book::images` for pictures referenced by this section, in order.
    pub images: Vec<usize>,
    /// Resolved archive path of this section's source document, for full-layout viewing.
    /// Empty for synthetic sections (e.g. PDF pages).
    pub href: String,
}
/// One decoded image resource, kept as raw encoded bytes (PNG/JPEG/GIF/…).
pub struct EpubImage {
    pub bytes: Vec<u8>,
}
pub struct Book {
    pub path: PathBuf,
    pub title: String,
    pub author: String,
    pub chapters: Vec<Chapter>,
    pub images: Vec<EpubImage>,
    pub blocked: Option<String>,
}

/// Read one ZIP entry as raw bytes, decrypting an ADEPT-encrypted resource when a key is
/// available. Used for images and other binary resources.
fn read_bytes_entry(
    zip: &mut ZipArchive<File>,
    name: &str,
    drm: Option<&Drm>,
) -> Result<Vec<u8>, String> {
    let mut raw = Vec::new();
    {
        let mut f = zip.by_name(name).map_err(|e| format!("{name}: {e}"))?;
        if f.size() > 64 * 1024 * 1024 {
            return Err(format!("{name}: entry exceeds 64 MiB"));
        }
        f.read_to_end(&mut raw)
            .map_err(|e| format!("{name}: {e}"))?;
    }
    if let Some(drm) = drm {
        if let Some(&compressed) = drm.resources.get(name) {
            return drm::decrypt_resource(&raw, &drm.key, compressed)
                .map_err(|e| format!("{name}: {e}"));
        }
    }
    Ok(raw)
}
fn xml(s: &str) -> Result<Document<'_>, String> {
    Document::parse_with_options(
        s,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 500_000,
            entity_resolver: None,
        },
    )
    .map_err(|e| e.to_string())
}
fn entry(zip: &mut ZipArchive<File>, name: &str) -> Result<String, String> {
    let f = zip.by_name(name).map_err(|e| format!("{name}: {e}"))?;
    if f.size() > 16 * 1024 * 1024 {
        return Err(format!("{name}: XML/text entry exceeds 16 MiB"));
    }
    let mut s = String::new();
    f.take(16 * 1024 * 1024 + 1)
        .read_to_string(&mut s)
        .map_err(|e| format!("{name}: {e}"))?;
    if s.len() > 16 * 1024 * 1024 {
        return Err("Entry exceeds size limit".into());
    }
    Ok(s)
}
fn resolve(base: &str, href: &str) -> Result<String, String> {
    let href = percent_encoding::percent_decode_str(href.split('#').next().unwrap_or(href))
        .decode_utf8()
        .map_err(|e| e.to_string())?;
    if href.contains(':') || href.starts_with('/') || href.contains('\\') {
        return Err("External EPUB resource is unsupported".into());
    }
    let mut parts: Vec<_> = base
        .rsplit_once('/')
        .map(|(dir, _)| dir.split('/').collect())
        .unwrap_or_default();
    for part in href.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err("EPUB path escapes archive".into());
                }
            }
            _ => parts.push(part),
        }
    }
    Ok(parts.join("/"))
}
fn text(node: Node<'_, '_>) -> String {
    node.descendants()
        .filter(|n| n.is_text())
        .filter_map(|n| n.text())
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
fn content(node: Node<'_, '_>, out: &mut String) {
    if node.is_text() {
        out.push_str(node.text().unwrap_or(""));
        return;
    }
    if matches!(node.tag_name().name(), "script" | "style" | "head") {
        return;
    }
    let name = node.tag_name().name();
    let block = matches!(
        name,
        "p" | "div"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "li"
            | "br"
            | "section"
            | "tr"
            | "blockquote"
            | "figure"
            | "figcaption"
    );
    if block {
        out.push('\n');
    }
    // Mark list items with a bullet so lists stay legible in the reflowed text view.
    if name == "li" {
        out.push_str("• ");
    }
    for child in node.children() {
        content(child, out);
    }
    if block {
        out.push('\n');
    }
}
pub fn open(path: &Path, license_key: Option<&RsaPrivateKey>) -> Result<Book, String> {
    let mut zip =
        ZipArchive::new(File::open(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let container = entry(&mut zip, "META-INF/container.xml")?;
    let doc = xml(&container)?;
    let opf_path = doc
        .descendants()
        .find(|n| n.has_tag_name("rootfile"))
        .and_then(|n| n.attribute("full-path"))
        .ok_or("Missing EPUB rootfile")?;
    let opf = entry(&mut zip, opf_path)?;
    let doc = xml(&opf)?;
    let field = |name| {
        doc.descendants()
            .find(|n| n.has_tag_name(name))
            .map(text)
            .unwrap_or_default()
    };
    let mut book = Book {
        path: path.to_owned(),
        title: field("title"),
        author: field("creator"),
        chapters: vec![],
        images: vec![],
        blocked: None,
    };
    if book.title.is_empty() {
        book.title = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
    }
    // Detect ADEPT protection and, when the device license key is available, unwrap the
    // content key and build the encrypted-resource map so protected books can be read.
    let has_rights = zip.by_name("META-INF/rights.xml").is_ok();
    let has_encryption = zip.by_name("META-INF/encryption.xml").is_ok();
    let mut drm: Option<Drm> = None;
    if has_rights || has_encryption {
        match license_key {
            Some(key) if has_rights && has_encryption => {
                let rights = entry(&mut zip, "META-INF/rights.xml")?;
                let encryption = entry(&mut zip, "META-INF/encryption.xml")?;
                match drm::content_key(&rights, key) {
                    Ok(content_key) => {
                        let mut resources = HashMap::new();
                        for resource in
                            drm::encrypted_resources(&encryption).map_err(|e| e.to_string())?
                        {
                            // Match CipherReference URIs to resolved archive paths.
                            let decoded = percent_encoding::percent_decode_str(&resource.uri)
                                .decode_utf8()
                                .map(|s| s.into_owned())
                                .unwrap_or(resource.uri);
                            resources.insert(decoded, resource.compressed);
                        }
                        drm = Some(Drm {
                            key: content_key,
                            resources,
                        });
                    }
                    Err(error) => {
                        book.blocked = Some(format!(
                            "This EPUB is protected and its content key could not be unwrapped with the current account ({error}). The original file is unchanged."
                        ));
                        return Ok(book);
                    }
                }
            }
            _ => {
                book.blocked = Some("This EPUB declares protected resources. Sign in with the account it was fulfilled to in order to read it. The original file is unchanged.".into());
                return Ok(book);
            }
        }
    }
    let manifest: HashMap<_, _> = doc
        .descendants()
        .filter(|n| n.has_tag_name("item"))
        .filter_map(|n| Some((n.attribute("id")?, n)))
        .collect();
    let mut titles = HashMap::new();
    for item in manifest.values().filter(|n| {
        n.attribute("properties")
            .unwrap_or("")
            .split_whitespace()
            .any(|p| p == "nav")
            || n.attribute("media-type") == Some("application/x-dtbncx+xml")
    }) {
        let nav_path = resolve(opf_path, item.attribute("href").unwrap_or(""))?;
        let nav = read_doc(&mut zip, &nav_path, drm.as_ref())?;
        let nav_doc = xml(&nav)?;
        for link in nav_doc.descendants().filter(|n| n.has_tag_name("a")) {
            if let Some(href) = link.attribute("href") {
                titles
                    .entry(resolve(&nav_path, href)?)
                    .or_insert_with(|| text(link));
            }
        }
        for point in nav_doc.descendants().filter(|n| n.has_tag_name("navPoint")) {
            let href = point
                .children()
                .find(|n| n.has_tag_name("content"))
                .and_then(|n| n.attribute("src"));
            let label = point.children().find(|n| n.has_tag_name("navLabel"));
            if let (Some(href), Some(label)) = (href, label) {
                titles
                    .entry(resolve(&nav_path, href)?)
                    .or_insert_with(|| text(label));
            }
        }
    }
    let mut total = 0usize;
    // Dedupe image resources across sections: archive path -> index into book.images.
    let mut image_index: HashMap<String, usize> = HashMap::new();
    for reference in doc
        .descendants()
        .filter(|n| n.has_tag_name("itemref") && n.attribute("linear") != Some("no"))
    {
        let item = manifest
            .get(reference.attribute("idref").unwrap_or(""))
            .ok_or("Spine references a missing manifest item")?;
        let name = resolve(
            opf_path,
            item.attribute("href").ok_or("Missing chapter href")?,
        )?;
        let source = read_doc(&mut zip, &name, drm.as_ref())?;
        total += source.len();
        if total > 64 * 1024 * 1024 {
            return Err("EPUB text exceeds 64 MiB reader limit".into());
        }
        let chapter = xml(&source)?;
        let title = titles
            .get(&name)
            .cloned()
            .or_else(|| {
                chapter
                    .descendants()
                    .find(|n| {
                        n.has_tag_name("h1") || n.has_tag_name("h2") || n.has_tag_name("title")
                    })
                    .map(text)
            })
            .unwrap_or_else(|| format!("Section {}", book.chapters.len() + 1));
        let mut raw = String::new();
        content(chapter.root_element(), &mut raw);
        let text = raw
            .lines()
            .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        // Collect image references (xhtml <img src> and SVG <image href>) in document
        // order, resolve each against this section, and decode-dedupe into book.images.
        let mut hrefs = Vec::new();
        for node in chapter.descendants() {
            let tag = node.tag_name().name();
            let href = if tag == "img" {
                node.attribute("src")
            } else if tag == "image" {
                node.attribute("href")
                    .or_else(|| node.attribute(("http://www.w3.org/1999/xlink", "href")))
            } else {
                None
            };
            if let Some(href) = href {
                if let Ok(resolved) = resolve(&name, href) {
                    hrefs.push(resolved);
                }
            }
        }
        drop(chapter);
        let mut images = Vec::new();
        for href in hrefs {
            let index = match image_index.get(&href) {
                Some(&index) => index,
                None => {
                    let Ok(bytes) = read_bytes_entry(&mut zip, &href, drm.as_ref()) else {
                        continue;
                    };
                    let index = book.images.len();
                    book.images.push(EpubImage { bytes });
                    image_index.insert(href, index);
                    index
                }
            };
            if !images.contains(&index) {
                images.push(index);
            }
        }
        book.chapters.push(Chapter {
            title,
            text,
            images,
            href: name.clone(),
        });
    }
    if book.chapters.is_empty() {
        return Err("EPUB has no readable spine sections".into());
    }
    Ok(book)
}

/// Unpack an EPUB's resources (decrypting ADEPT-encrypted ones with the license key)
/// into `out_dir`, preserving archive paths, so the chapter files can be opened in a
/// webview for full CSS layout. The original file is never modified. Returns an error if
/// the book is protected but no usable key is available.
pub fn extract_to_dir(
    path: &Path,
    license_key: Option<&RsaPrivateKey>,
    out_dir: &Path,
) -> Result<(), String> {
    let mut zip =
        ZipArchive::new(File::open(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    // Rebuild the decryption map, mirroring open().
    let has_rights = zip.by_name("META-INF/rights.xml").is_ok();
    let has_encryption = zip.by_name("META-INF/encryption.xml").is_ok();
    let mut drm: Option<Drm> = None;
    if has_rights || has_encryption {
        match license_key {
            Some(key) if has_rights && has_encryption => {
                let rights = entry(&mut zip, "META-INF/rights.xml")?;
                let encryption = entry(&mut zip, "META-INF/encryption.xml")?;
                let content_key = drm::content_key(&rights, key).map_err(|e| e.to_string())?;
                let mut resources = HashMap::new();
                for resource in drm::encrypted_resources(&encryption).map_err(|e| e.to_string())? {
                    let decoded = percent_encoding::percent_decode_str(&resource.uri)
                        .decode_utf8()
                        .map(|s| s.into_owned())
                        .unwrap_or(resource.uri);
                    resources.insert(decoded, resource.compressed);
                }
                drm = Some(Drm {
                    key: content_key,
                    resources,
                });
            }
            _ => return Err("protected EPUB requires the fulfilling account".into()),
        }
    }
    let names: Vec<String> = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_owned()))
        .collect();
    for name in names {
        if name.ends_with('/') {
            continue;
        }
        // Reject entries that would escape the output directory.
        if name.contains("..") || name.starts_with('/') || name.contains('\\') {
            continue;
        }
        let bytes = read_bytes_entry(&mut zip, &name, drm.as_ref())?;
        let dest = out_dir.join(&name);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&dest, bytes).map_err(|e| e.to_string())?;
    }
    Ok(())
}
