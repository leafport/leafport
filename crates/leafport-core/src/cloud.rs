//! ADEPT cloud library requests: download the account's ACSM list, upload new ACSMs.
//! These functions construct signed requests; they do not advance sync state.

use base64::{Engine, engine::general_purpose::STANDARD};
use rsa::RsaPrivateKey;
use std::{error::Error, fmt};

use crate::signing::{RequestSigningError, sign_request};

pub const PRODUCTION_BASE: &str = "https://ebook.adobe.com/ebookcloud";
pub const DOWNLOAD_OPERATION: &str = "SyncToDevice";
pub const UPLOAD_OPERATION: &str = "SyncToCloud";

#[derive(Debug)]
pub enum CloudError {
    Endpoint,
    Signing(RequestSigningError),
    Transport(crate::service::ServiceError),
    Http {
        status: reqwest::StatusCode,
        body: Vec<u8>,
    },
    Utf8(std::string::FromUtf8Error),
    Response(CloudResponseError),
}

impl fmt::Display for CloudError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Endpoint => write!(
                f,
                "cloud base must be an HTTP(S) URL without credentials, query, or fragment"
            ),
            Self::Signing(error) => error.fmt(f),
            Self::Transport(error) => error.fmt(f),
            Self::Http { status, .. } => write!(f, "cloud HTTP request failed: {status}"),
            Self::Utf8(error) => write!(f, "cloud response is not UTF-8: {error}"),
            Self::Response(error) => error.fmt(f),
        }
    }
}
impl Error for CloudError {}

/// Validate a cloud base URL and normalise its path to end in `/`.
fn endpoint(base: &str) -> Result<reqwest::Url, CloudError> {
    let mut base = reqwest::Url::parse(base).map_err(|_| CloudError::Endpoint)?;
    if !matches!(base.scheme(), "http" | "https")
        || base.host_str().is_none()
        || !base.username().is_empty()
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
    {
        return Err(CloudError::Endpoint);
    }
    let path = format!("{}/", base.path().trim_end_matches('/'));
    base.set_path(&path);
    Ok(base)
}

fn exchange<T>(
    transport: &dyn crate::service::Transport,
    base: &reqwest::Url,
    operation: &str,
    request: String,
    parse: impl FnOnce(&str) -> Result<T, CloudResponseError>,
) -> Result<T, CloudError> {
    let url = base.join(operation).map_err(|_| CloudError::Endpoint)?;
    let response = transport
        .post_adept(url.as_str(), request)
        .map_err(CloudError::Transport)?;
    if !response.status.is_success() {
        // Preserve semantic ADEPT errors when available, otherwise retain bytes.
        if let Ok(xml) = std::str::from_utf8(&response.body)
            && let Err(error @ CloudResponseError::Service { .. }) = parse(xml)
        {
            return Err(CloudError::Response(error));
        }
        return Err(CloudError::Http {
            status: response.status,
            body: response.body,
        });
    }
    let xml = String::from_utf8(response.body).map_err(CloudError::Utf8)?;
    parse(&xml).map_err(CloudError::Response)
}

/// Raw SyncToDevice response plus its decoded records, so callers can cache the XML.
#[derive(Clone, Debug)]
pub struct SyncToDeviceResponse {
    pub xml: String,
    pub records: Vec<DownloadRecord>,
}

/// SyncToDevice without date bounds, over any transport.
pub fn sync_to_device(
    transport: &dyn crate::service::Transport,
    base: &str,
    user: &str,
    key: &RsaPrivateKey,
) -> Result<SyncToDeviceResponse, CloudError> {
    let base = endpoint(base)?;
    let request = download_request(user, None, None, key).map_err(CloudError::Signing)?;
    let xml = exchange(transport, &base, DOWNLOAD_OPERATION, request, |xml| {
        download_records(xml).map(|records| (xml.to_owned(), records))
    })?;
    Ok(SyncToDeviceResponse {
        xml: xml.0,
        records: xml.1,
    })
}

/// SyncToCloud over any transport; Ok only on the explicit SUCCESS acknowledgment.
pub fn sync_to_cloud(
    transport: &dyn crate::service::Transport,
    base: &str,
    user: &str,
    acsm: &[u8],
    key: &RsaPrivateKey,
) -> Result<(), CloudError> {
    let base = endpoint(base)?;
    let request = upload_request(user, acsm, key).map_err(CloudError::Signing)?;
    exchange(
        transport,
        &base,
        UPLOAD_OPERATION,
        request,
        acknowledge_upload,
    )
}

/// The `data` attribute of an ADEPT `error` root element.
pub fn adept_error_data(xml: &str) -> Option<String> {
    roxmltree::Document::parse(xml)
        .ok()
        .and_then(|document| document.root_element().attribute("data").map(str::to_owned))
}

#[derive(Debug)]
pub enum CloudResponseError {
    TooLarge,
    Xml(roxmltree::Error),
    /// Retain the complete server error rather than discard provider-specific fields.
    Service {
        xml: String,
    },
    InvalidResponse,
}

impl fmt::Display for CloudResponseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => write!(f, "cloud response exceeds the local size limit"),
            Self::Xml(error) => write!(f, "E_ADEPT_XML_SYNTAX: {error}"),
            Self::Service { xml } => {
                // The ADEPT error root carries its code and context in the `data` attribute.
                let data = adept_error_data(xml);
                match data {
                    Some(data) => write!(f, "cloud service returned an ADEPT error: {data}"),
                    None => write!(f, "cloud service returned an ADEPT error"),
                }
            }
            Self::InvalidResponse => write!(f, "E_ADEPT_INVALID_RESPONSE SyncToCloud"),
        }
    }
}

impl Error for CloudResponseError {}

#[derive(Clone, Debug)]
pub struct DownloadRecord {
    pub user: String,
    pub acsm: Vec<u8>,
}

/// Decode returned records without committing library changes or advancing dates.
/// Entries missing either user or acsm, or whose acsm is not valid Base64, are
/// skipped; unknown children are ignored.
pub fn download_records(xml: &str) -> Result<Vec<DownloadRecord>, CloudResponseError> {
    const NS: &str = "http://ns.adobe.com/adept";
    if xml.len() > 4 * 1024 * 1024 {
        return Err(CloudResponseError::TooLarge);
    }
    let document = roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 50_000,
            entity_resolver: None,
        },
    )
    .map_err(CloudResponseError::Xml)?;
    if document.root_element().has_tag_name((NS, "error")) {
        return Err(CloudResponseError::Service {
            xml: xml.to_owned(),
        });
    }
    let mut records = Vec::new();
    if let Some(container) = document
        .descendants()
        .find(|node| node.has_tag_name((NS, "synctodeviceResult")))
    {
        for record in container
            .children()
            .filter(|node| node.has_tag_name((NS, "user2acsm")))
        {
            let field = |name| {
                record
                    .children()
                    .find(|node| node.has_tag_name((NS, name)))
                    .map(|node| {
                        node.descendants()
                            .filter(|node| node.is_text())
                            .filter_map(|node| node.text())
                            .collect::<String>()
                    })
            };
            if let (Some(user), Some(encoded)) = (field("user"), field("acsm")) {
                let encoded: String = encoded
                    .chars()
                    .filter(|c| !c.is_ascii_whitespace())
                    .collect();
                let Ok(acsm) = STANDARD.decode(encoded) else {
                    continue;
                };
                records.push(DownloadRecord { user, acsm });
            }
        }
    }
    Ok(records)
}

/// Accept the explicit upload acknowledgment.
/// HTTP success alone must never acknowledge a queued upload.
pub fn acknowledge_upload(xml: &str) -> Result<(), CloudResponseError> {
    if xml.len() > 4 * 1024 * 1024 {
        return Err(CloudResponseError::TooLarge);
    }
    let document = roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 50_000,
            entity_resolver: None,
        },
    )
    .map_err(CloudResponseError::Xml)?;
    let root = document.root_element();
    if root.has_tag_name(("http://ns.adobe.com/adept", "error")) {
        return Err(CloudResponseError::Service {
            xml: xml.to_owned(),
        });
    }
    let text: String = root
        .descendants()
        .filter(|node| node.is_text())
        .filter_map(|node| node.text())
        .collect();
    if root.has_tag_name(("http://ns.adobe.com/adept", "synctocloudResult"))
        && text.trim() == "SUCCESS"
    {
        Ok(())
    } else {
        Err(CloudResponseError::InvalidResponse)
    }
}

/// Preserve the server's date strings rather than applying local time zones.
/// Missing/empty bounds are omitted.
pub fn download_request(
    user: &str,
    from_date: Option<&str>,
    to_date: Option<&str>,
    signing_key: &RsaPrivateKey,
) -> Result<String, RequestSigningError> {
    let mut xml = String::from("<synctodevice xmlns=\"http://ns.adobe.com/adept\">");
    field(&mut xml, "user", user);
    for (name, value) in [("fromDate", from_date), ("toDate", to_date)] {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            field(&mut xml, name, value);
        }
    }
    xml.push_str("</synctodevice>");
    sign_request(&xml, signing_key)
}

/// Upload the original ACSM bytes, Base64 encoded.
/// Callers retain the bytes until the service explicitly acknowledges success.
pub fn upload_request(
    user: &str,
    acsm: &[u8],
    signing_key: &RsaPrivateKey,
) -> Result<String, RequestSigningError> {
    // Bound allocation before Base64 expansion. The shared signer also bounds XML.
    if acsm.len() > 3 * 1024 * 1024 {
        return Err(RequestSigningError::TooLarge);
    }
    let mut xml = String::from("<synctocloud xmlns=\"http://ns.adobe.com/adept\">");
    field(&mut xml, "user", user);
    field(&mut xml, "acsm", &STANDARD.encode(acsm));
    xml.push_str("</synctocloud>");
    sign_request(&xml, signing_key)
}

pub(crate) fn field(xml: &mut String, name: &str, value: &str) {
    xml.push('<');
    xml.push_str(name);
    xml.push('>');
    for character in value.chars() {
        match character {
            '&' => xml.push_str("&amp;"),
            '<' => xml.push_str("&lt;"),
            '>' => xml.push_str("&gt;"),
            // Preserve CR through XML's mandatory line-ending normalization.
            '\r' => xml.push_str("&#13;"),
            character => xml.push(character),
        }
    }
    xml.push_str("</");
    xml.push_str(name);
    xml.push('>');
}
