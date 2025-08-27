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
use crate::download_engine::http::http_client::HttpClient;

pub mod byte_range;
pub mod http_download_engine;
pub mod http_download_worker;
pub mod message;
pub mod progress;
mod http_client;

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

pub struct FileInfo {
    pub url: String,
    pub file_size: u64,
    pub file_name: String,
    pub supports_range: bool,
}

pub async fn fetch_file_info(url: String) -> anyhow::Result<FileInfo> {
    let uri: Uri = url.parse()?;
    let client = HttpClient::new();
    let make_req = |method: &str| {
        Request::builder()
            .method(method)
            .uri(uri.clone())
            .header("User-Agent", "rust-hyper/1.0") // TODO fix
            .body(Empty::new())
    };
    let mut resp = client.send(make_req("HEAD")?).await?;
    let mut file_size = extract_content_length(&resp);
    let mut file_name = extract_file_name(&resp);
    let mut supports_range = extract_range_support(&resp);
    if file_size.is_none() && file_name.is_none() && !supports_range {
        let get_resp = client.send(make_req("GET")?).await?;
        resp = get_resp;

        file_size = extract_content_length(&resp);
        file_name = extract_file_name(&resp);
        supports_range = extract_range_support(&resp);
        drop(resp);
    }
    Ok(FileInfo {
        url: url.to_string(),
        file_name: file_name
            .or_else(|| extract_file_name_from_url(url.clone()))
            .unwrap(), // TODO: fix
        file_size: file_size.expect("REASON"),
        supports_range,
    })
}

fn extract_file_name_from_url(url: String) -> Option<String> {
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
            let filename = filename
                .strip_prefix('"')
                .and_then(|f| f.strip_suffix('"'))
                .unwrap_or(filename);
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
