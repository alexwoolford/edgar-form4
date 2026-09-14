//! Blocking SEC GETs. Tests inject a map; live ingest sleeps and retries.

use std::collections::HashMap;
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};

pub const TICKERS_URL: &str = "https://www.sec.gov/files/company_tickers_exchange.json";
pub const ARCHIVES_PREFIX: &str = "https://www.sec.gov/Archives/";
pub const MIN_SLEEP_SECS: f64 = 0.1;
pub const DEFAULT_SLEEP_SECS: f64 = 0.5;

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
        if sleep_secs < MIN_SLEEP_SECS {
            bail!(
                "sleep {sleep_secs}s is below {MIN_SLEEP_SECS}s (SEC 10 req/s ceiling; do not rotate UA)"
            );
        }
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
}

impl Fetcher for LiveFetcher {
    fn get(&mut self, url: &str) -> Result<HttpResponse> {
        if !self.first {
            thread::sleep(self.sleep);
        }
        self.first = false;
        let mut last_err = None;
        for attempt in 0..4u32 {
            if attempt > 0 {
                thread::sleep(Duration::from_millis(500 * 2u64.pow(attempt - 1)));
            }
            let resp = self
                .client
                .get(url)
                .send()
                .with_context(|| format!("GET {url}"))?;
            let status = resp.status().as_u16();
            if matches!(status, 429 | 500 | 502 | 503 | 504) && attempt < 3 {
                last_err = Some(status);
                tracing::warn!(url, status, attempt, "retryable SEC status");
                continue;
            }
            let body = resp.text().with_context(|| format!("read body {url}"))?;
            return Ok(HttpResponse { status, body });
        }
        bail!("GET {url} failed after retries (last status {last_err:?})")
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
