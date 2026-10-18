//! Adobe ADEPT (EBX_HANDLER) PDF decryption.
//!
//! Fulfilled PDFs carry a PDF `/Encrypt` dictionary with `/Filter /EBX_HANDLER`. The content
//! key is unwrapped from the fulfillment licenseToken with the device license key, the
//! same RSA PKCS#1 v1.5 path as the EPUB rights key. EBX encrypts each stream and string
//! with AES-128-CBC under that key, the IV prepended to the ciphertext.
//!
//! Not yet verified against a live DRM PDF. Decrypting yields a plain PDF whose text
//! layer is then read.

use std::{error::Error, fmt};

use lopdf::{Document, LoadOptions, Object, ObjectStream, xref::XrefEntry};

/// Same-length stand-in for the trailer's `/Encrypt` key. lopdf 0.42 routes any document
/// whose trailer has `/Encrypt` through its own decryption path, which drops every object
/// for filters it does not know (EBX_HANDLER). Renaming the key in place keeps all xref
/// offsets valid and makes lopdf load the objects raw.
const HIDDEN_ENCRYPT: &[u8] = b"NoCrypt";
/// Type given to object streams while loading, so lopdf keeps the still-encrypted
/// container instead of failing to expand it and dropping it.
const HIDDEN_OBJSTM: &[u8] = b"EbxObjStm";

#[derive(Debug)]
pub enum PdfDrmError {
    Parse(String),
    /// The PDF is not EBX-encrypted (no EBX_HANDLER /Encrypt filter).
    NotEbx,
    /// A stream/string was shorter than one AES block plus its IV.
    Truncated,
    Decrypt(String),
}

impl fmt::Display for PdfDrmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(error) => write!(f, "PDF parse failed: {error}"),
            Self::NotEbx => write!(f, "PDF is not EBX_HANDLER-encrypted"),
            Self::Truncated => write!(f, "encrypted PDF object is too short for an IV"),
            Self::Decrypt(error) => write!(f, "PDF decryption failed: {error}"),
        }
    }
}
impl Error for PdfDrmError {}

/// Returns true when `pdf` declares an Adobe EBX_HANDLER `/Encrypt` filter.
pub fn is_ebx_encrypted(pdf: &[u8]) -> bool {
    match load_raw(pdf) {
        Ok(document) => ebx_present(&document),
        Err(_) => false,
    }
}

/// Load `pdf` without lopdf's decryption: the trailer `/Encrypt` key is renamed to
/// [`HIDDEN_ENCRYPT`] and object streams are retyped to [`HIDDEN_OBJSTM`].
fn load_raw(pdf: &[u8]) -> Result<Document, PdfDrmError> {
    Document::load_mem_with_options(
        &hide_encrypt_key(pdf),
        LoadOptions::with_filter(hide_object_stream),
    )
    .map_err(|e| PdfDrmError::Parse(e.to_string()))
}

/// Rename every `/Encrypt` name outside stream data to `/NoCrypt`. That covers classic
/// trailers and xref stream dictionaries, whose dictionaries precede their stream data.
/// Stream bodies are skipped so ciphertext is never touched.
fn hide_encrypt_key(pdf: &[u8]) -> Vec<u8> {
    const KEY: &[u8] = b"/Encrypt";
    let is_regular = |byte: u8| byte.is_ascii_alphanumeric() || b"#_.-+".contains(&byte);
    let mut out = pdf.to_vec();
    let mut i = 0;
    while i < out.len() {
        let rest = &out[i..];
        let after_stream = rest.get(6).copied();
        if rest.starts_with(b"stream")
            && (i == 0 || !out[i - 1].is_ascii_alphanumeric())
            && matches!(after_stream, Some(b'\r' | b'\n'))
        {
            match find(&out[i + 6..], b"endstream") {
                Some(end) => i += 6 + end + 9,
                None => break,
            }
            continue;
        }
        if rest.starts_with(KEY) && !rest.get(KEY.len()).is_some_and(|&b| is_regular(b)) {
            out[i + 1..i + KEY.len()].copy_from_slice(HIDDEN_ENCRYPT);
            i += KEY.len();
            continue;
        }
        i += 1;
    }
    out
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Load filter: retype object streams so lopdf keeps them for expansion after decryption.
fn hide_object_stream(id: (u32, u16), object: &mut Object) -> Option<((u32, u16), Object)> {
    if let Object::Stream(stream) = object
        && stream.dict.has_type(b"ObjStm")
    {
        stream
            .dict
            .set("Type", Object::Name(HIDDEN_OBJSTM.to_vec()));
    }
    Some((id, object.clone()))
}

fn ebx_present(document: &Document) -> bool {
    let Ok(encrypt) = document.trailer.get(HIDDEN_ENCRYPT) else {
        return false;
    };
    let dict = match encrypt {
        Object::Reference(id) => document.get_object(*id).and_then(|o| o.as_dict()).ok(),
        Object::Dictionary(dict) => Some(dict),
        _ => None,
    };
    dict.and_then(|dict| dict.get(b"Filter").ok())
        .and_then(|filter| filter.as_name().ok())
        .is_some_and(|name| name == b"EBX_HANDLER")
}

fn aes_cbc_decrypt(data: &[u8], key: &[u8; 16]) -> Result<Vec<u8>, PdfDrmError> {
    use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
    if data.len() < 32 || (data.len() - 16) % 16 != 0 {
        return Err(PdfDrmError::Truncated);
    }
    let (iv, ciphertext) = data.split_at(16);
    let mut buffer = ciphertext.to_vec();
    // EBX pads with PKCS#7; fall back to the raw blocks if the padding is not present.
    let plain = cbc::Decryptor::<aes::Aes128>::new(key.into(), iv.into())
        .decrypt_padded_mut::<Pkcs7>(&mut buffer)
        .map(<[u8]>::to_vec)
        .unwrap_or(buffer);
    Ok(plain)
}

/// Decrypt an EBX_HANDLER PDF with the unwrapped 16-byte content key, returning plain PDF
/// bytes with the `/Encrypt` dictionary removed. Streams and strings are decrypted with
/// AES-128-CBC (IV prepended). Objects that do not decrypt cleanly are left as-is so a
/// partially recoverable document still parses.
pub fn decrypt_pdf(pdf: &[u8], content_key: &[u8; 16]) -> Result<Vec<u8>, PdfDrmError> {
    let mut document = load_raw(pdf)?;
    if !ebx_present(&document) {
        return Err(PdfDrmError::NotEbx);
    }
    // The /Encrypt object itself, xref streams and the document ID are never encrypted.
    let encrypt_id = match document.trailer.get(HIDDEN_ENCRYPT) {
        Ok(Object::Reference(id)) => Some(*id),
        _ => None,
    };
    for (id, object) in document.objects.iter_mut() {
        let is_xref = object
            .as_stream()
            .is_ok_and(|stream| stream.dict.has_type(b"XRef"));
        if Some(*id) != encrypt_id && !is_xref {
            decrypt_object(object, content_key);
        }
    }
    expand_object_streams(&mut document);
    if let Some(id) = encrypt_id {
        document.objects.remove(&id);
    }
    document.trailer.remove(HIDDEN_ENCRYPT);
    let mut output = Vec::new();
    document
        .save_to(&mut output)
        .map_err(|e| PdfDrmError::Decrypt(e.to_string()))?;
    Ok(output)
}

/// Unpack the decrypted object streams into top-level objects. As in lopdf's own loader,
/// an object is taken only from the container its xref entry names, so stale copies from
/// earlier revisions do not win. Containers that do not parse are kept as plain streams.
fn expand_object_streams(document: &mut Document) {
    let containers: Vec<_> = document
        .objects
        .iter()
        .filter(|(_, object)| {
            object
                .as_stream()
                .is_ok_and(|s| s.dict.has_type(HIDDEN_OBJSTM))
        })
        .map(|(id, _)| *id)
        .collect();
    for container_id in containers {
        let Some(Object::Stream(stream)) = document.objects.get_mut(&container_id) else {
            continue;
        };
        stream.dict.set("Type", Object::Name(b"ObjStm".to_vec()));
        let Ok(object_stream) = ObjectStream::new(stream) else {
            continue;
        };
        for (id, object) in object_stream.objects {
            let in_this_container = matches!(
                document.reference_table.get(id.0),
                Some(XrefEntry::Compressed { container, .. }) if *container == container_id.0
            );
            if in_this_container {
                document.objects.insert(id, object);
            }
        }
        document.objects.remove(&container_id);
    }
}

/// Decrypt the content of one object in place: stream bytes, strings, and the members of
/// arrays and dictionaries, recursively. Decryption failures leave the value unchanged.
fn decrypt_object(object: &mut Object, key: &[u8; 16]) {
    match object {
        Object::Stream(stream) => {
            if let Ok(plain) = aes_cbc_decrypt(&stream.content, key) {
                stream.set_content(plain);
            }
        }
        Object::String(bytes, _) => {
            if let Ok(plain) = aes_cbc_decrypt(bytes, key) {
                *bytes = plain;
            }
        }
        Object::Array(items) => {
            for item in items {
                decrypt_object(item, key);
            }
        }
        Object::Dictionary(dict) => {
            for (_, value) in dict.iter_mut() {
                decrypt_object(value, key);
            }
        }
        _ => {}
    }
}
