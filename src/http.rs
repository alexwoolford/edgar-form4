//! Blocking SEC GETs. Tests inject a map; live ingest sleeps and retries.

use std::collections::HashMap;
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};

pub const TICKERS_URL: &str = "https://www.sec.gov/files/company_tickers_exchange.json";
pub const ARCHIVES_PREFIX: &str = "https://www.sec.gov/Archives/";
pub const MIN_SLEEP_SECS: f64 = 0.1;
pub const DEFAULT_SLEEP_SECS: f64 = 0.5;
pub const MAX_BODY_BYTES: u64 = 32 * 1024 * 1024;
const ATTEMPTS: u32 = 4;

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

pub trait Fetcher {
    fn get(&mut self, url: &str) -> Result<HttpResponse>;
}

/// In-memory fetcher for `cargo test` (no network).
#[derive(Debug, Default)]
pub struct MapFetcher {
    pub urls: HashMap<String, HttpResponse>,
}

impl Fetcher for MapFetcher {
    fn get(&mut self, url: &str) -> Result<HttpResponse> {
        self.urls
            .get(url)
            .cloned()
            .with_context(|| format!("test fetcher missing {url}"))
    }
}

pub struct LiveFetcher {
    client: reqwest::blocking::Client,
    sleep: Duration,
    first: bool,
}

impl LiveFetcher {
    pub fn new(user_agent: &str, sleep_secs: f64) -> Result<Self> {
        validate_sleep(sleep_secs)?;
        let client = reqwest::blocking::Client::builder()
            .user_agent(user_agent)
            .gzip(true)
            .timeout(Duration::from_secs(60))
            .build()
            .context("reqwest client")?;
        Ok(Self {
            client,
            sleep: Duration::from_secs_f64(sleep_secs),
            first: true,
        })
    }

    fn attempt(&self, url: &str) -> Result<HttpResponse> {
        let resp = self
            .client
            .get(url)
            .send()
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status().as_u16();
        if let Some(len) = resp.content_length() {
            check_body_len(len, url)?;
        }
        let bytes = resp.bytes().with_context(|| format!("read body {url}"))?;
        check_body_len(bytes.len() as u64, url)?;
        let body = String::from_utf8_lossy(&bytes).into_owned();
        Ok(HttpResponse { status, body })
    }
}

impl Fetcher for LiveFetcher {
    fn get(&mut self, url: &str) -> Result<HttpResponse> {
        if !self.first {
            thread::sleep(self.sleep);
        }
        self.first = false;
        let mut last_err = None;
        for attempt in 0..ATTEMPTS {
            if attempt > 0 {
                thread::sleep(Duration::from_millis(500 * 2u64.pow(attempt - 1)));
            }
            match self.attempt(url) {
                Ok(resp) if is_retryable_status(resp.status) && attempt + 1 < ATTEMPTS => {
                    tracing::warn!(url, status = resp.status, attempt, "retryable SEC status");
                    last_err = Some(format!("status {}", resp.status));
                }
                Ok(resp) => return Ok(resp),
                Err(err) if attempt + 1 < ATTEMPTS => {
                    tracing::warn!(url, attempt, error = %err, "retryable SEC transport error");
                    last_err = Some(err.to_string());
                }
                Err(err) => return Err(err),
            }
        }
        bail!("GET {url} failed after retries (last error {last_err:?})")
    }
}

pub fn filing_url(filename: &str) -> String {
    let f = filename.trim().trim_start_matches('/');
    format!("{ARCHIVES_PREFIX}{f}")
}

pub fn validate_sleep(sleep_secs: f64) -> Result<()> {
    if sleep_secs < MIN_SLEEP_SECS {
        bail!(
            "sleep {sleep_secs}s is below {MIN_SLEEP_SECS}s (SEC 10 req/s ceiling; do not rotate UA)"
        );
    }
    Ok(())
}

pub fn is_retryable_status(status: u16) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 504)
}

pub fn check_body_len(len: u64, url: &str) -> Result<()> {
    if len > MAX_BODY_BYTES {
        bail!("GET {url} body {len} bytes exceeds {MAX_BODY_BYTES} cap");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_throttle_and_server_errors_not_client_errors() {
        assert!(is_retryable_status(429));
        assert!(is_retryable_status(500));
        assert!(is_retryable_status(502));
        assert!(is_retryable_status(503));
        assert!(is_retryable_status(504));
        assert!(!is_retryable_status(200));
        assert!(!is_retryable_status(403));
        assert!(!is_retryable_status(404));
    }

    #[test]
    fn body_cap_rejects_oversize() {
        check_body_len(MAX_BODY_BYTES, "https://www.sec.gov/x").unwrap();
        let err = check_body_len(MAX_BODY_BYTES + 1, "https://www.sec.gov/x").unwrap_err();
        assert!(err.to_string().contains("exceeds"));
    }

    #[test]
    fn validate_sleep_enforces_floor() {
        validate_sleep(0.5).unwrap();
        assert!(validate_sleep(0.05).is_err());
    }
}
