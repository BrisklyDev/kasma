use http_body_util::{BodyExt, Empty};
use hyper::body::{Bytes, Incoming};
use hyper::header::{ACCEPT_RANGES, CONTENT_DISPOSITION, CONTENT_LENGTH};
use hyper::{Request, Response, Uri};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioTimer};
use std::collections::HashMap;
use std::error::Error as StdError;
use std::fmt;
use std::time::Duration;

pub mod http_download_engine;
pub mod http_download_worker;
pub mod segment;

#[derive(Debug)]
pub enum ClientError {
    Transport(hyper_util::client::legacy::Error),
    Other(String),
}

impl From<hyper_util::client::legacy::Error> for ClientError {
    fn from(err: hyper_util::client::legacy::Error) -> Self {
        ClientError::Transport(err)
    }
}

impl From<hyper::http::Error> for ClientError {
    fn from(err: hyper::http::Error) -> Self {
        ClientError::Other(err.to_string())
    }
}

impl From<&str> for ClientError {
    fn from(err: &str) -> Self {
        ClientError::Other(err.to_string())
    }
}

impl From<String> for ClientError {
    fn from(err: String) -> Self {
        ClientError::Other(err)
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientError::Transport(e) => write!(f, "Transport error: {}", e),
            ClientError::Other(e) => write!(f, "{}", e),
        }
    }
}

impl StdError for ClientError {}

pub struct HttpClient {
    inner: Client<HttpsConnector<HttpConnector>, Empty<Bytes>>,
}

impl HttpClient {
    pub fn new() -> Self {
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()
            .unwrap()
            .https_or_http()
            .enable_http1()
            .build();

        let client: Client<HttpsConnector<HttpConnector>, Empty<Bytes>> =
            Client::builder(TokioExecutor::new())
                .pool_timer(TokioTimer::new())
                .pool_idle_timeout(Duration::from_secs(30))
                .build(https);

        HttpClient { inner: client }
    }

    /// Sends request with redirect support. Does not support body
    pub async fn send(
        &self,
        request: Request<Empty<Bytes>>,
    ) -> Result<Response<Incoming>, ClientError> {
        let mut current_url = request.uri().clone();
        let mut redirect_count = 0;
        let current_request = request;
        const MAX_REDIRECTS: u8 = 10;
        loop {
            let mut req_builder = Request::builder()
                .method(current_request.method())
                .uri(current_url.clone())
                .version(current_request.version());
            for header in current_request.headers() {
                req_builder = req_builder.header(header.0, header.1);
            }
            let req = req_builder.body(Empty::<Bytes>::new())?;
            let resp = self.inner.request(req).await?;
            match resp.status() {
                status if status.is_success() => {
                    return Ok(resp);
                }
                status if status.is_redirection() => {
                    redirect_count += 1;
                    if redirect_count > MAX_REDIRECTS {
                        return Err("Too many redirects".into());
                    }

                    let location = resp
                        .headers()
                        .get("location")
                        .ok_or("Redirect response missing Location header")?
                        .to_str()
                        .map_err(|_| "Invalid redirect location")?;

                    println!("Following redirect to: {}", location);
                    // Handle both absolute and relative URLs
                    current_url = if location.starts_with("http") {
                        location
                            .parse()
                            .map_err(|e| format!("Invalid URI: {}", e))?
                    } else {
                        let mut parts = current_url.into_parts();
                        parts.path_and_query = Some(
                            location
                                .parse()
                                .map_err(|e| format!("Invalid path: {}", e))?,
                        );
                        Uri::from_parts(parts).map_err(|e| format!("Invalid URI parts: {}", e))?
                    };
                }
                status => {
                    return Err(format!("HTTP error: {}", status).into());
                }
            }
        }
    }
}

pub struct FileInfo {
    pub url: String,
    pub headers: HashMap<String, String>,
    pub file_size: u64,
    pub file_name: String,
    pub supports_range: bool,
}

pub async fn fetch_file_info(url: &str) -> Result<FileInfo, Box<dyn std::error::Error>> {
    let uri: Uri = url.parse()?;
    let client = HttpClient::new();
    // Construct base request builder
    let make_req = |method: &str| {
        Request::builder()
            .method(method)
            .uri(uri.clone())
            .header("User-Agent", "rust-hyper/1.0") // TODO fix
            .body(Empty::new())
    };
    // Send HEAD request first
    let mut resp = client.send(make_req("HEAD")?).await?;
    let mut file_size = extract_content_length(&resp);
    let mut file_name = extract_file_name(&resp);
    let mut supports_range = extract_range_support(&resp);
    // If important headers are missing, retry with GET
    if file_size.is_none() && file_name.is_none() && !supports_range {
        let get_resp = client.send(make_req("GET")?).await?;
        resp = get_resp;

        // Re-extract
        file_size = extract_content_length(&resp);
        file_name = extract_file_name(&resp);
        supports_range = extract_range_support(&resp);
        drop(resp);
    }
    Ok(FileInfo {
        url: url.to_string(),
        headers: HashMap::new(),
        file_name: file_name
            .or_else(|| extract_file_name_from_url(url))
            .unwrap(), // TODO fix
        file_size: file_size.expect("REASON"),
        supports_range,
    })
}

fn extract_file_name_from_url(url: &str) -> Option<String> {
    url.split('/')
        .last()
        .map(|s| s.split('?').next().unwrap_or("").to_string())
        .filter(|s| !s.is_empty())
}

fn extract_content_length(resp: &hyper::Response<hyper::body::Incoming>) -> Option<u64> {
    resp.headers()
        .get(CONTENT_LENGTH)
        .and_then(|len| len.to_str().ok()?.parse::<u64>().ok())
}

/// Extracts the filename from a `Content-Disposition` header.
pub fn extract_filename(resp: &Response<Incoming>) -> Option<String> {
    let header_value = resp.headers().get(CONTENT_DISPOSITION)?.to_str().ok()?;
    for token in header_value.split(';') {
        let token = token.trim();
        if token.to_lowercase().starts_with("filename") {
            let filename = token.splitn(2, '=').nth(1)?.trim();
            // Remove surrounding quotes if any
            let filename = filename
                .strip_prefix('"')
                .and_then(|f| f.strip_suffix('"'))
                .unwrap_or(filename);
            // Remove UTF-8 prefix if present
            return Some(
                filename
                    .strip_prefix("UTF-8''")
                    .unwrap_or(filename)
                    .to_string(),
            );
        }
    }
    None
}

fn extract_file_name(resp: &hyper::Response<hyper::body::Incoming>) -> Option<String> {
    resp.headers().get(CONTENT_DISPOSITION).and_then(|val| {
        let val = val.to_str().ok()?;
        val.split(';').find_map(|part| {
            let part = part.trim();
            if part.to_lowercase().starts_with("filename=") {
                Some(
                    part.trim_start_matches("filename=")
                        .trim_matches('"')
                        .to_string(),
                )
            } else {
                None
            }
        })
    })
}

fn extract_range_support(resp: &hyper::Response<hyper::body::Incoming>) -> bool {
    resp.headers()
        .get(ACCEPT_RANGES)
        .and_then(|v| v.to_str().ok())
        .map_or(false, |v| v.to_ascii_lowercase().contains("bytes"))
}
