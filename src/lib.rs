//! Nightly EDGAR Form 4 / 4/A open-market purchases (transaction code P).
//! A label, not a lead: the crate does not emit a cluster score.

pub mod db;
pub mod http;
pub mod index;
pub mod ingest;
pub mod ownership;
pub mod sec_ua;
pub mod tickers;
pub mod time;

pub use db::{open, open_work, WorkDb, DB_NAME};
pub use ingest::{ingest_dates, ingest_day, ingest_range, IngestStats};
pub use sec_ua::{validate_user_agent, UserAgentError};
pub use time::{utc_iso, INSTANT_FMT};
