//! HTTP access shared by the desktop application and diagnostic harness.

use std::{error::Error, fmt, io::Read, time::Duration};

use crate::vendors::{MAX_SERVICE_INFO_BYTES, VendorCatalog, VendorError};

/// Authentication service discovery endpoint.
pub const AUTHENTICATION_SERVICE_INFO: &str =
    "https://adeactivate.adobe.com/adept/AuthenticationServiceInfo";

#[derive(Debug)]
pub enum ServiceError {
    ResponseTooLarge,
    /// The server answered 429 Too Many Requests. Not retried: retrying only prolongs
    /// a rate limit or block.
    RateLimited {
        host: String,
    },
    Http(reqwest::Error),
    Read(std::io::Error),
    Utf8(std::string::FromUtf8Error),
    Vendor(VendorError),
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResponseTooLarge => write!(f, "service response exceeds the local size limit"),
            Self::RateLimited { host } => write!(
                f,
                "{host} is limiting requests from this network (HTTP 429). Try again later."
            ),
            Self::Http(error) => write!(f, "service request failed: {error}"),
            Self::Read(error) => write!(f, "service response read failed: {error}"),
            Self::Utf8(error) => write!(f, "service response is not UTF-8: {error}"),
            Self::Vendor(error) => error.fmt(f),
        }
    }
}

impl Error for ServiceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        if matches!(self, Self::ResponseTooLarge | Self::RateLimited { .. }) {
            return None;
        }
        Some(match self {
            Self::ResponseTooLarge | Self::RateLimited { .. } => unreachable!(),
            Self::Http(error) => error,
            Self::Read(error) => error,
            Self::Utf8(error) => error,
            Self::Vendor(error) => error,
        })
    }
}

pub struct ServiceClient {
    http: reqwest::blocking::Client,
}

/// Retain error bodies: ADEPT services may return protocol XML on HTTP failure.
pub struct AdeptResponse {
    pub status: reqwest::StatusCode,
    pub body: Vec<u8>,
}

/// Seam over the blocking HTTP calls so protocol flows can run against a fake.
pub trait Transport: Send + Sync {
    fn post_adept(&self, url: &str, xml: String) -> Result<AdeptResponse, ServiceError>;
    fn fetch(&self, url: &str, max_bytes: usize) -> Result<AdeptResponse, ServiceError>;
}

impl Transport for ServiceClient {
    fn post_adept(&self, url: &str, xml: String) -> Result<AdeptResponse, ServiceError> {
        ServiceClient::post_adept(self, url, xml)
    }

    fn fetch(&self, url: &str, max_bytes: usize) -> Result<AdeptResponse, ServiceError> {
        ServiceClient::fetch(self, url, max_bytes)
    }
}

/// User-Agent sent with every request. Kobo's Cloudflare front end refuses requests
/// without one with 403.
const USER_AGENT: &str = concat!("Leafport/", env!("CARGO_PKG_VERSION"));

/// Turn a 429 into `RateLimited`, naming the host the request ended at.
fn reject_rate_limit(
    response: reqwest::blocking::Response,
) -> Result<reqwest::blocking::Response, ServiceError> {
    if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(ServiceError::RateLimited {
            host: response.url().host_str().unwrap_or_default().to_owned(),
        });
    }
    Ok(response)
}

impl ServiceClient {
    /// Blocking transport for already constructed ADEPT request XML.
    /// No automatic retry: fulfillment may consume a single-use token.
    pub fn post_adept(&self, url: &str, xml: String) -> Result<AdeptResponse, ServiceError> {
        let response = self
            .http
            .post(url)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/vnd.adobe.adept+xml",
            )
            .body(xml)
            .send()
            .map_err(ServiceError::Http)?;
        let response = reject_rate_limit(response)?;
        let status = response.status();
        let mut body = Vec::new();
        response
            .take(MAX_SERVICE_INFO_BYTES as u64 + 1)
            .read_to_end(&mut body)
            .map_err(ServiceError::Read)?;
        if body.len() > MAX_SERVICE_INFO_BYTES {
            return Err(ServiceError::ResponseTooLarge);
        }
        Ok(AdeptResponse { status, body })
    }

    /// Download a fulfilled resource (the book bytes) from a fulfillment href.
    /// Bounded by `max_bytes`; the status and body are retained so an error page is
    /// visible to the caller rather than silently treated as book content.
    pub fn fetch(&self, url: &str, max_bytes: usize) -> Result<AdeptResponse, ServiceError> {
        let response = self.http.get(url).send().map_err(ServiceError::Http)?;
        let response = reject_rate_limit(response)?;
        let status = response.status();
        let mut body = Vec::new();
        response
            .take(max_bytes as u64 + 1)
            .read_to_end(&mut body)
            .map_err(ServiceError::Read)?;
        if body.len() > max_bytes {
            return Err(ServiceError::ResponseTooLarge);
        }
        Ok(AdeptResponse { status, body })
    }

    pub fn new() -> Result<Self, ServiceError> {
        let http = reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(15))
            // Fulfilled downloads can be large; allow a longer overall deadline.
            .timeout(Duration::from_secs(300))
            // ADEPT relies on signatures, so http is allowed; cap redirect chains
            // and refuse https -> http downgrades and non-http(s) schemes.
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() > 5 {
                    attempt.error("too many redirects")
                } else if !matches!(attempt.url().scheme(), "http" | "https") {
                    attempt.error("redirect to non-http(s) scheme")
                } else if attempt.url().scheme() == "http"
                    && attempt
                        .previous()
                        .last()
                        .is_some_and(|p| p.scheme() == "https")
                {
                    attempt.error("https to http redirect downgrade")
                } else {
                    attempt.follow()
                }
            }))
            .build()
            .map_err(ServiceError::Http)?;
        Ok(Self { http })
    }

    /// Blocking operation: desktop callers must run this off the UI thread.
    pub fn load_vendors(&self, catalog: &mut VendorCatalog) -> Result<(), ServiceError> {
        let response = self
            .http
            .get(AUTHENTICATION_SERVICE_INFO)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(ServiceError::Http)?;
        let mut bytes = Vec::new();
        response
            .take(MAX_SERVICE_INFO_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(ServiceError::Read)?;
        if bytes.len() > MAX_SERVICE_INFO_BYTES {
            return Err(ServiceError::Vendor(VendorError::InputTooLarge));
        }
        let xml = String::from_utf8(bytes).map_err(ServiceError::Utf8)?;
        catalog
            .apply_service_info(&xml)
            .map_err(ServiceError::Vendor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One request against a local server: the raw request head it received.
    fn request_head(send: impl FnOnce(&ServiceClient, &str)) -> String {
        respond_with(
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            send,
        )
    }

    /// One request against a local server answering `response`: the request head.
    fn respond_with(response: &'static str, send: impl FnOnce(&ServiceClient, &str)) -> String {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/x", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut head = Vec::new();
            let mut byte = [0];
            while !head.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                head.push(byte[0]);
            }
            stream.write_all(response.as_bytes()).unwrap();
            String::from_utf8(head).unwrap()
        });
        send(&ServiceClient::new().unwrap(), &url);
        server.join().unwrap()
    }

    #[test]
    fn a_429_fails_at_once_as_rate_limited() {
        const LIMITED: &str =
            "HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        respond_with(LIMITED, |client, url| {
            let Err(error) = client.fetch(url, 1024) else {
                panic!("429 accepted");
            };
            assert!(matches!(error, ServiceError::RateLimited { ref host } if host == "127.0.0.1"));
            assert_eq!(
                error.to_string(),
                "127.0.0.1 is limiting requests from this network (HTTP 429). Try again later."
            );
        });
        respond_with(LIMITED, |client, url| {
            let Err(error) = client.post_adept(url, String::new()) else {
                panic!("429 accepted");
            };
            assert!(matches!(error, ServiceError::RateLimited { .. }));
        });
    }

    #[test]
    fn requests_carry_the_user_agent() {
        // Kobo's Cloudflare front end answers requests without a User-Agent with 403.
        for head in [
            request_head(|client, url| {
                client.fetch(url, 1024).unwrap();
            }),
            request_head(|client, url| {
                client.post_adept(url, String::new()).unwrap();
            }),
        ] {
            assert!(
                head.to_ascii_lowercase().contains(&format!(
                    "\r\nuser-agent: leafport/{}\r\n",
                    env!("CARGO_PKG_VERSION")
                )),
                "{head}"
            );
        }
    }
}
