//! Light-weight reading of ACSM files for library bookkeeping.

use percent_encoding::percent_decode_str;
use sha1::{Digest, Sha1};

use super::LibraryError;
use crate::fulfillment::{AcsmError, MAX_ACSM_BYTES};

const ADEPT: &str = "http://ns.adobe.com/adept";
const DC: &str = "http://purl.org/dc/elements/1.1/";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Epub,
    Pdf,
    Unknown,
}

impl Format {
    pub fn from_mime(mime: &str) -> Format {
        match mime.trim() {
            "application/epub+zip" => Format::Epub,
            "application/pdf" => Format::Pdf,
            _ => Format::Unknown,
        }
    }

    pub fn sniff(bytes: &[u8]) -> Format {
        if bytes.starts_with(b"%PDF") {
            Format::Pdf
        } else if bytes.starts_with(b"PK\x03\x04") {
            Format::Epub
        } else {
            Format::Unknown
        }
    }

    pub fn extension(self) -> Option<&'static str> {
        match self {
            Format::Epub => Some("epub"),
            Format::Pdf => Some("pdf"),
            Format::Unknown => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Format::Epub => "EPUB",
            Format::Pdf => "PDF",
            Format::Unknown => "?",
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Format::Epub => "application/epub+zip",
            Format::Pdf => "application/pdf",
            Format::Unknown => "application/octet-stream",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AcsmSummary {
    pub key: String,
    pub resource: Option<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub format: Format,
    pub src: Option<String>,
}

fn text_of(node: roxmltree::Node) -> Option<String> {
    let text: String = node
        .descendants()
        .filter(|n| n.is_text())
        .filter_map(|n| n.text())
        .collect();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn find_text(root: roxmltree::Node, ns: &str, name: &str) -> Option<String> {
    root.descendants()
        .filter(|n| n.has_tag_name((ns, name)))
        .find_map(text_of)
}

/// Reads identity and display metadata from an ACSM without validating it for fulfillment.
pub fn acsm_summary(bytes: &[u8]) -> Result<AcsmSummary, LibraryError> {
    if bytes.len() > MAX_ACSM_BYTES {
        return Err(AcsmError::TooLarge.into());
    }
    let xml = String::from_utf8_lossy(bytes);
    let document = roxmltree::Document::parse_with_options(
        &xml,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 50_000,
            entity_resolver: None,
        },
    )
    .map_err(AcsmError::Xml)?;
    let root = document.root();
    let resource = find_text(root, ADEPT, "resource");
    let src = find_text(root, ADEPT, "src");
    let meta = root
        .descendants()
        .find(|n| n.has_tag_name((ADEPT, "metadata")))
        .unwrap_or(root);
    let title = find_text(meta, DC, "title");
    let author = find_text(meta, DC, "creator");
    let format = find_text(meta, DC, "format")
        .map(|m| Format::from_mime(&m))
        .unwrap_or(Format::Unknown);
    let key = match &resource {
        Some(r) => r.clone(),
        None => {
            let digest = Sha1::digest(bytes);
            let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
            format!("sha1:{hex}")
        }
    };
    Ok(AcsmSummary {
        key,
        resource,
        title,
        author,
        format,
        src,
    })
}

/// File name of a download URL without query, extension or percent-encoding.
pub fn src_title(src: &str) -> Option<String> {
    let path = src.split(['?', '#']).next().unwrap_or(src);
    let segment = path.rsplit('/').next().unwrap_or(path);
    let decoded = percent_decode_str(segment).decode_utf8_lossy();
    let stem = match decoded.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => &decoded,
    };
    let stem = stem.trim();
    (!stem.is_empty()).then(|| stem.to_owned())
}
