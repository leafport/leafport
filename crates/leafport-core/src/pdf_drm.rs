//! Adobe ADEPT (EBX_HANDLER) PDF decryption.
//!
//! Fulfilled PDFs carry a PDF `/Encrypt` dictionary with `/Filter /EBX_HANDLER`. Its
//! `/ADEPT_LICENSE` string is the Base64 of the raw-deflated rights XML; the book key is
//! the ADEPT `encryptedKey` from it, unwrapped with the device license key by RSA
//! PKCS#1 v1.5 — the same path as the EPUB rights key. Each top-level object is then
//! decrypted with its own key, derived as in the standard PDF security handler:
//! MD5(book key ‖ object number (3 bytes LE) ‖ generation (2 bytes LE) ‖ `sAlT` for AES),
//! truncated to min(book key length + 5, 16) bytes. `/V` selects the cipher: V 1-3 is RC4;
//! V 4 takes the stream and string ciphers from the crypt filters named by `/StmF` and
//! `/StrF` (`/CFM /AESV2` is AES-128-CBC with the IV prepended and PKCS#7 padding, `/V2` is
//! RC4, `/None` or `/Identity` leaves values as-is); V 5 / AESV3 is not supported.
//!
//! Not yet verified against a live DRM PDF. Decrypting yields a plain PDF whose text
//! layer is then read.

use std::{error::Error, fmt};

use lopdf::{Dictionary, Document, LoadOptions, Object, ObjectId, ObjectStream, xref::XrefEntry};
use rsa::{Pkcs1v15Encrypt, RsaPrivateKey};

/// Same-length stand-in for the trailer's `/Encrypt` key. lopdf 0.42 routes any document
/// whose trailer has `/Encrypt` through its own decryption path, which drops every object
/// for filters it does not know (EBX_HANDLER). Renaming the key in place keeps all xref
/// offsets valid and makes lopdf load the objects raw.
const HIDDEN_ENCRYPT: &[u8] = b"NoCrypt";
/// Type given to object streams while loading, so lopdf keeps the still-encrypted
/// container instead of failing to expand it and dropping it.
const HIDDEN_OBJSTM: &[u8] = b"EbxObjStm";
const ADEPT: &str = "http://ns.adobe.com/adept";
/// Upper bound on the inflated `/ADEPT_LICENSE` rights XML.
const MAX_LICENSE_BYTES: u64 = 1024 * 1024;

#[derive(Debug)]
pub enum PdfDrmError {
    Parse(String),
    /// The PDF is not EBX-encrypted (no EBX_HANDLER /Encrypt filter).
    NotEbx,
    /// A stream/string was shorter than one AES block plus its IV.
    Truncated,
    Decrypt(String),
    /// The /Encrypt dictionary has no usable /ADEPT_LICENSE string.
    MissingLicense,
    /// /ADEPT_LICENSE did not decode into rights XML holding a wrapped key.
    License(String),
    /// The book key length does not suit the cipher selected by /V.
    KeyLength(usize),
    /// The /Encrypt dictionary asks for a scheme this handler does not implement.
    Unsupported(String),
}

impl fmt::Display for PdfDrmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(error) => write!(f, "PDF parse failed: {error}"),
            Self::NotEbx => write!(f, "PDF is not EBX_HANDLER-encrypted"),
            Self::Truncated => write!(f, "encrypted PDF object is too short for an IV"),
            Self::Decrypt(error) => write!(f, "PDF decryption failed: {error}"),
            Self::MissingLicense => write!(f, "EBX /Encrypt dictionary has no /ADEPT_LICENSE"),
            Self::License(error) => write!(f, "ADEPT_LICENSE content key unwrap failed: {error}"),
            Self::KeyLength(length) => {
                write!(
                    f,
                    "book key is {length} bytes, invalid for this PDF's cipher"
                )
            }
            Self::Unsupported(what) => write!(f, "unsupported PDF encryption: {what}"),
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

fn encrypt_dict(document: &Document) -> Option<&Dictionary> {
    match document.trailer.get(HIDDEN_ENCRYPT).ok()? {
        Object::Reference(id) => document.get_object(*id).and_then(|o| o.as_dict()).ok(),
        Object::Dictionary(dict) => Some(dict),
        _ => None,
    }
}

fn ebx_present(document: &Document) -> bool {
    encrypt_dict(document)
        .and_then(|dict| dict.get(b"Filter").ok())
        .and_then(|filter| filter.as_name().ok())
        .is_some_and(|name| name == b"EBX_HANDLER")
}

/// Unwrap the book key from the EBX `/Encrypt` dictionary's `/ADEPT_LICENSE`: Base64 of
/// the raw-deflated rights XML whose ADEPT `encryptedKey` is RSA PKCS#1 v1.5-wrapped to
/// the device license key.
pub fn content_key(pdf: &[u8], license_key: &RsaPrivateKey) -> Result<Vec<u8>, PdfDrmError> {
    use std::io::Read;

    use base64::{Engine, engine::general_purpose::STANDARD};
    let document = load_raw(pdf)?;
    if !ebx_present(&document) {
        return Err(PdfDrmError::NotEbx);
    }
    let Some(Ok(Object::String(license, _))) =
        encrypt_dict(&document).map(|dict| dict.get(b"ADEPT_LICENSE"))
    else {
        return Err(PdfDrmError::MissingLicense);
    };
    let encoded: Vec<u8> = license
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    let compressed = STANDARD
        .decode(encoded)
        .map_err(|e| PdfDrmError::License(e.to_string()))?;
    let mut rights_xml = String::new();
    flate2::read::DeflateDecoder::new(compressed.as_slice())
        .take(MAX_LICENSE_BYTES)
        .read_to_string(&mut rights_xml)
        .map_err(|e| PdfDrmError::License(e.to_string()))?;
    unwrap_license_key(&rights_xml, license_key)
}

/// Unwrap the book key from ADEPT rights XML: its `encryptedKey` is Base64 of the key
/// RSA PKCS#1 v1.5-wrapped to the device license key.
pub fn unwrap_license_key(
    rights_xml: &str,
    license_key: &RsaPrivateKey,
) -> Result<Vec<u8>, PdfDrmError> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let document = roxmltree::Document::parse_with_options(
        rights_xml,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 50_000,
            entity_resolver: None,
        },
    )
    .map_err(|e| PdfDrmError::License(e.to_string()))?;
    let wrapped: String = document
        .descendants()
        .find(|node| node.has_tag_name((ADEPT, "encryptedKey")))
        .ok_or_else(|| PdfDrmError::License("rights XML has no encryptedKey".into()))?
        .descendants()
        .filter(|node| node.is_text())
        .filter_map(|node| node.text())
        .collect::<String>()
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    let wrapped = STANDARD
        .decode(wrapped)
        .map_err(|e| PdfDrmError::License(e.to_string()))?;
    let mut key = license_key
        .decrypt(Pkcs1v15Encrypt, &wrapped)
        .map_err(|e| PdfDrmError::License(e.to_string()))?;
    // As in ineptpdf: a longer key whose 17th-from-last byte is 0 carries the book key last.
    if key.len() > 16 && key[key.len() - 17] == 0 {
        key.drain(..key.len() - 16);
    }
    Ok(key)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cipher {
    /// Values are stored in the clear.
    Identity,
    Rc4,
    Aes,
}

/// The ciphers the `/Encrypt` dictionary selects for streams and for strings.
struct Ciphers {
    stream: Cipher,
    string: Cipher,
    /// False when `/EncryptMetadata false`: `/Type /Metadata` streams are stored plain.
    encrypt_metadata: bool,
}

// Open question: ineptpdf treats EBX books differently from this standard-handler reading
// (its V 3 `genkey_v3`, decrypting EBX as RC4 regardless of /V, and a /Length-based key
// size with a V prefix). None of that is confirmed by a real book, so it is not
// implemented here; in particular a V 4 EBX PDF without /StmF and /StrF is read as Identity.
/// V 1-3 is RC4 throughout; V 4 resolves `/StmF` and `/StrF` through `/CF`; V 5 is
/// rejected as unsupported.
fn ciphers_of(dict: &Dictionary) -> Result<Ciphers, PdfDrmError> {
    let version = dict.get(b"V").and_then(Object::as_i64).unwrap_or(0);
    let encrypt_metadata = dict
        .get(b"EncryptMetadata")
        .and_then(Object::as_bool)
        .unwrap_or(true);
    let (stream, string) = match version {
        ..=3 => (Cipher::Rc4, Cipher::Rc4),
        4 => (crypt_filter(dict, b"StmF")?, crypt_filter(dict, b"StrF")?),
        _ => return Err(PdfDrmError::Unsupported(format!("/V {version}"))),
    };
    Ok(Ciphers {
        stream,
        string,
        encrypt_metadata,
    })
}

/// Cipher of the crypt filter named by `key` (`/StmF` or `/StrF`); missing means Identity.
fn crypt_filter(dict: &Dictionary, key: &[u8]) -> Result<Cipher, PdfDrmError> {
    let name = match dict.get(key) {
        Ok(name) => name
            .as_name()
            .map_err(|_| PdfDrmError::Parse(format!("/{} is not a name", ascii(key))))?,
        Err(_) => return Ok(Cipher::Identity),
    };
    if name == b"Identity" {
        return Ok(Cipher::Identity);
    }
    let filter = dict
        .get(b"CF")
        .and_then(Object::as_dict)
        .and_then(|filters| filters.get(name))
        .and_then(Object::as_dict)
        .map_err(|_| {
            PdfDrmError::Unsupported(format!("crypt filter /{} is not in /CF", ascii(name)))
        })?;
    match filter.get(b"CFM").and_then(Object::as_name) {
        Err(_) | Ok(b"None") => Ok(Cipher::Identity),
        Ok(b"V2") => Ok(Cipher::Rc4),
        Ok(b"AESV2") => Ok(Cipher::Aes),
        Ok(other) => Err(PdfDrmError::Unsupported(format!("/CFM /{}", ascii(other)))),
    }
}

fn ascii(name: &[u8]) -> String {
    String::from_utf8_lossy(name).into_owned()
}

/// Per-object key of the standard PDF security handler.
fn object_key(book_key: &[u8], id: ObjectId, cipher: Cipher) -> Vec<u8> {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(book_key);
    hasher.update(&id.0.to_le_bytes()[..3]);
    hasher.update(id.1.to_le_bytes());
    if cipher == Cipher::Aes {
        hasher.update(b"sAlT");
    }
    let digest = hasher.finalize();
    digest[..(book_key.len() + 5).min(16)].to_vec()
}

fn rc4_apply<N>(key: &[u8], data: &[u8]) -> Result<Vec<u8>, PdfDrmError>
where
    N: rc4::cipher::generic_array::ArrayLength<u8>,
{
    use rc4::{KeyInit, Rc4, StreamCipher};
    let mut cipher =
        Rc4::<N>::new_from_slice(key).map_err(|_| PdfDrmError::KeyLength(key.len()))?;
    let mut buffer = data.to_vec();
    cipher.apply_keystream(&mut buffer);
    Ok(buffer)
}

fn rc4_decrypt(data: &[u8], key: &[u8]) -> Result<Vec<u8>, PdfDrmError> {
    use rc4::consts::{U10, U11, U12, U13, U14, U15, U16};
    match key.len() {
        10 => rc4_apply::<U10>(key, data),
        11 => rc4_apply::<U11>(key, data),
        12 => rc4_apply::<U12>(key, data),
        13 => rc4_apply::<U13>(key, data),
        14 => rc4_apply::<U14>(key, data),
        15 => rc4_apply::<U15>(key, data),
        16 => rc4_apply::<U16>(key, data),
        length => Err(PdfDrmError::KeyLength(length)),
    }
}

fn decrypt_value(data: &[u8], key: &[u8], cipher: Cipher) -> Result<Vec<u8>, PdfDrmError> {
    match cipher {
        Cipher::Identity => Ok(data.to_vec()),
        Cipher::Rc4 => rc4_decrypt(data, key),
        Cipher::Aes => {
            let key = key
                .try_into()
                .map_err(|_| PdfDrmError::KeyLength(key.len()))?;
            aes_cbc_decrypt(data, key)
        }
    }
}

fn aes_cbc_decrypt(data: &[u8], key: &[u8; 16]) -> Result<Vec<u8>, PdfDrmError> {
    use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
    if data.len() < 32 || !(data.len() - 16).is_multiple_of(16) {
        return Err(PdfDrmError::Truncated);
    }
    let (iv, ciphertext) = data.split_at(16);
    let mut buffer = ciphertext.to_vec();
    // Bad padding means a wrong key or a value that was never encrypted: fail closed.
    let plain = cbc::Decryptor::<aes::Aes128>::new(key.into(), iv.into())
        .decrypt_padded_mut::<Pkcs7>(&mut buffer)
        .map_err(|_| PdfDrmError::Decrypt("invalid AES padding".into()))?;
    Ok(plain.to_vec())
}

/// Decrypt an EBX_HANDLER PDF with the unwrapped book key (see [`content_key`]), returning
/// plain PDF bytes with the `/Encrypt` dictionary removed. Each object's streams and strings
/// are decrypted with its per-object key under the ciphers [`ciphers_of`] selects. The book
/// key must be 16 bytes for AES, 5..=16 for RC4.
///
/// Fails closed: an AES value with bad padding (or too short for its IV) is an error naming
/// the object. RC4 cannot detect a wrong key per value, so after decryption every RC4
/// stream with a leading `/FlateDecode` filter is test-inflated; if there are any and none
/// inflates, the key is taken to be wrong.
pub fn decrypt_pdf(pdf: &[u8], content_key: &[u8]) -> Result<Vec<u8>, PdfDrmError> {
    let mut document = load_raw(pdf)?;
    let Some(ciphers) = encrypt_dict(&document)
        .filter(|_| ebx_present(&document))
        .map(ciphers_of)
    else {
        return Err(PdfDrmError::NotEbx);
    };
    let ciphers = ciphers?;
    let key_ok = |cipher| match cipher {
        Cipher::Identity => true,
        Cipher::Aes => content_key.len() == 16,
        Cipher::Rc4 => (5..=16).contains(&content_key.len()),
    };
    if !key_ok(ciphers.stream) || !key_ok(ciphers.string) {
        return Err(PdfDrmError::KeyLength(content_key.len()));
    }
    // The /Encrypt object itself, xref streams and the document ID are never encrypted.
    let encrypt_id = match document.trailer.get(HIDDEN_ENCRYPT) {
        Ok(Object::Reference(id)) => Some(*id),
        _ => None,
    };
    let (mut flate_streams, mut flate_ok) = (0usize, 0usize);
    for (id, object) in document.objects.iter_mut() {
        let is_xref = object
            .as_stream()
            .is_ok_and(|stream| stream.dict.has_type(b"XRef"));
        if Some(*id) == encrypt_id || is_xref {
            continue;
        }
        let crypt = ObjectCrypt {
            stream: (object_key(content_key, *id, ciphers.stream), ciphers.stream),
            string: (object_key(content_key, *id, ciphers.string), ciphers.string),
            encrypt_metadata: ciphers.encrypt_metadata,
        };
        let stream_decrypted = decrypt_object(object, &crypt)
            .map_err(|e| PdfDrmError::Decrypt(format!("object {} {}: {e}", id.0, id.1)))?;
        if stream_decrypted
            && ciphers.stream == Cipher::Rc4
            && let Object::Stream(stream) = object
            && first_filter_is_flate(&stream.dict)
        {
            flate_streams += 1;
            if inflates(&stream.content) {
                flate_ok += 1;
            }
        }
    }
    if flate_streams > 0 && flate_ok == 0 {
        return Err(PdfDrmError::Decrypt(
            "wrong key: no FlateDecode stream inflates".into(),
        ));
    }
    expand_object_streams(&mut document)?;
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

fn first_filter_is_flate(dict: &Dictionary) -> bool {
    let first = match dict.get(b"Filter") {
        Ok(Object::Array(filters)) => filters.first(),
        Ok(filter) => Some(filter),
        Err(_) => None,
    };
    first
        .and_then(|filter| filter.as_name().ok())
        .is_some_and(|name| name == b"FlateDecode")
}

/// True when the first few KB of `data` inflate as zlib. A wrong RC4 key yields noise that
/// fails the zlib header check or the deflate block structure almost immediately.
fn inflates(data: &[u8]) -> bool {
    use std::io::Read;
    let mut sink = Vec::new();
    flate2::read::ZlibDecoder::new(data)
        .take(4096)
        .read_to_end(&mut sink)
        .is_ok()
}

/// Unpack the decrypted object streams into top-level objects. As in lopdf's own loader,
/// an object is taken only from the container its xref entry names, so stale copies from
/// earlier revisions do not win. A container that does not parse is an error, since its
/// compressed objects would otherwise be lost.
fn expand_object_streams(document: &mut Document) -> Result<(), PdfDrmError> {
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
        let object_stream = ObjectStream::new(stream).map_err(|e| {
            PdfDrmError::Decrypt(format!(
                "object stream {} did not parse: {e}",
                container_id.0
            ))
        })?;
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
    Ok(())
}

/// Per-object keys and ciphers for streams and strings.
struct ObjectCrypt {
    stream: (Vec<u8>, Cipher),
    string: (Vec<u8>, Cipher),
    encrypt_metadata: bool,
}

/// Decrypt the content of one object in place: stream bytes, strings (including those in a
/// stream's dictionary), and the members of arrays and dictionaries, recursively. Returns
/// whether stream content was decrypted; a value that fails to decrypt is an error.
fn decrypt_object(object: &mut Object, crypt: &ObjectCrypt) -> Result<bool, PdfDrmError> {
    match object {
        Object::Stream(stream) => {
            decrypt_dict(&mut stream.dict, crypt)?;
            let plain_metadata = !crypt.encrypt_metadata && stream.dict.has_type(b"Metadata");
            if plain_metadata || crypt.stream.1 == Cipher::Identity {
                return Ok(false);
            }
            let plain = decrypt_value(&stream.content, &crypt.stream.0, crypt.stream.1)?;
            stream.set_content(plain);
            return Ok(true);
        }
        Object::String(bytes, _) => {
            *bytes = decrypt_value(bytes, &crypt.string.0, crypt.string.1)?;
        }
        Object::Array(items) => {
            for item in items {
                decrypt_object(item, crypt)?;
            }
        }
        Object::Dictionary(dict) => decrypt_dict(dict, crypt)?,
        _ => {}
    }
    Ok(false)
}

fn decrypt_dict(dict: &mut Dictionary, crypt: &ObjectCrypt) -> Result<(), PdfDrmError> {
    for (_, value) in dict.iter_mut() {
        decrypt_object(value, crypt)?;
    }
    Ok(())
}
