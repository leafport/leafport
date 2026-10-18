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

use lopdf::{Document, Object};

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
    match Document::load_mem(pdf) {
        Ok(document) => ebx_present(&document),
        Err(_) => false,
    }
}

fn ebx_present(document: &Document) -> bool {
    let Ok(encrypt) = document.trailer.get(b"Encrypt") else {
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
    let mut document = Document::load_mem(pdf).map_err(|e| PdfDrmError::Parse(e.to_string()))?;
    if !ebx_present(&document) {
        return Err(PdfDrmError::NotEbx);
    }
    // The /Encrypt object itself and the document ID are never encrypted.
    let encrypt_id = match document.trailer.get(b"Encrypt") {
        Ok(Object::Reference(id)) => Some(*id),
        _ => None,
    };
    let ids: Vec<_> = document.objects.keys().copied().collect();
    for id in ids {
        if Some(id) == encrypt_id {
            continue;
        }
        if let Some(object) = document.objects.get_mut(&id) {
            decrypt_object(object, content_key);
        }
    }
    document.trailer.remove(b"Encrypt");
    let mut output = Vec::new();
    document
        .save_to(&mut output)
        .map_err(|e| PdfDrmError::Decrypt(e.to_string()))?;
    Ok(output)
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
