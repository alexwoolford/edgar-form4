//! One UTC calendar day of Form 4 / 4/A open-market purchases (code P).

use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::{Datelike, Days, NaiveDate, Utc, Weekday};

use crate::db::{upsert_purchase, upsert_run, WorkDb};
use crate::http::{filing_url, Fetcher, TICKERS_URL};
use crate::index::{master_index_url, normalize_accession, parse_master_index, IndexRow};
use crate::ownership::parse_purchases;
use crate::tickers::{parse_tickers_json, primary_listings};

#[derive(Debug, Clone, Default)]
pub struct IngestStats {
    pub status: String,
    pub filings_seen: i64,
    pub filings_upserted: i64,
    pub filings_failed: i64,
    pub txt_ok: i64,
    pub index_url: String,
}

pub fn ingest_day(
    db: &mut WorkDb,
    date: NaiveDate,
    fetcher: &mut dyn Fetcher,
) -> Result<IngestStats> {
    let started = Utc::now();
    let as_of = date.format("%Y-%m-%d").to_string();
    let index_url = master_index_url(date);
    let mut stats = IngestStats {
        index_url: index_url.clone(),
        status: "ok".into(),
        ..IngestStats::default()
    };

    let idx_resp = fetcher.get(&index_url).context("GET master index")?;
    if index_absent_is_ok(date, idx_resp.status) {
        tracing::info!(
            status = idx_resp.status,
            date = %as_of,
            "index missing; closed market"
        );
        stats.status = "ok".into();
        finish(db, &as_of, started, &stats)?;
        return Ok(stats);
    }
    if idx_resp.status != 200 {
        stats.status = "error".into();
        finish(db, &as_of, started, &stats)?;
        anyhow::bail!("master index HTTP {} for {index_url}", idx_resp.status);
    }

    let rows = parse_master_index(&idx_resp.body);
    stats.filings_seen = rows.len() as i64;

    let tickers = if rows.is_empty() {
        HashMap::new()
    } else {
        match load_tickers(fetcher) {
            Ok(t) => t,
            Err(err) => {
                stats.status = "error".into();
                finish(db, &as_of, started, &stats)?;
                return Err(err).context("company_tickers_exchange.json");
            }
        }
    };

    let tx = db.unchecked_transaction()?;
    for row in &rows {
        match resolve_filing(fetcher, row, &tickers, &mut stats) {
            Ok(purchases) => {
                for p in purchases {
                    if upsert_purchase(&tx, &p)? {
                        stats.filings_upserted += 1;
                    }
                }
            }
            Err(err) => {
                tracing::warn!(
                    cik = %row.cik,
                    filename = %row.filename,
                    error = %err,
                    "filing failed"
                );
                stats.filings_failed += 1;
            }
        }
    }
    if stats.filings_failed > 0 {
        stats.status = "partial".into();
    }
    upsert_run(
        &tx,
        &as_of,
        started,
        Utc::now(),
        &stats.status,
        &stats.index_url,
        stats.filings_seen,
        stats.filings_upserted,
        stats.filings_failed,
        stats.txt_ok,
    )?;
    tx.commit()?;
    db.nudge.send();
    Ok(stats)
}

fn finish(
    db: &mut WorkDb,
    as_of: &str,
    started: chrono::DateTime<Utc>,
    stats: &IngestStats,
) -> Result<()> {
    let tx = db.unchecked_transaction()?;
    upsert_run(
        &tx,
        as_of,
        started,
        Utc::now(),
        &stats.status,
        &stats.index_url,
        stats.filings_seen,
        stats.filings_upserted,
        stats.filings_failed,
        stats.txt_ok,
    )?;
    tx.commit()?;
    db.nudge.send();
    Ok(())
}

fn load_tickers(fetcher: &mut dyn Fetcher) -> Result<HashMap<String, String>> {
    let resp = fetcher
        .get(TICKERS_URL)
        .context("GET company_tickers_exchange.json")?;
    if resp.status != 200 {
        anyhow::bail!("tickers HTTP {}", resp.status);
    }
    let companies = parse_tickers_json(resp.body.as_bytes())?;
    Ok(primary_listings(&companies))
}

fn resolve_filing(
    fetcher: &mut dyn Fetcher,
    row: &IndexRow,
    tickers: &HashMap<String, String>,
    stats: &mut IngestStats,
) -> Result<Vec<crate::ownership::Purchase>> {
    let accession = accession_from_filename(&row.filename);
    let url = filing_url(&row.filename);
    let txt = fetcher.get(&url)?;
    if txt.status != 200 {
        anyhow::bail!("filing HTTP {} for {url}", txt.status);
    }
    stats.txt_ok += 1;
    let ticker = tickers.get(&row.cik).cloned();
    Ok(parse_purchases(
        &txt.body,
        &accession,
        &row.cik,
        &row.company_name,
        &row.form,
        &row.filed_date,
        &row.filename,
        ticker,
    ))
}

/// Missing daily index: 404 always, or 403 on Sat/Sun UTC (OCI often 403s
/// unpublished weekend paths instead of 404). Weekday 403 is still an error.
fn index_absent_is_ok(date: NaiveDate, status: u16) -> bool {
    match status {
        404 => true,
        403 => matches!(date.weekday(), Weekday::Sat | Weekday::Sun),
        _ => false,
    }
}

fn accession_from_filename(filename: &str) -> String {
    let base = filename.rsplit('/').next().unwrap_or(filename);
    let stem = base.strip_suffix(".txt").unwrap_or(base);
    normalize_accession(stem)
}

/// Parse `YYYY-MM-DD`. Used by the CLI.
pub fn parse_as_of(s: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").with_context(|| format!("date {s}"))
}

pub fn default_as_of() -> NaiveDate {
    Utc::now()
        .date_naive()
        .checked_sub_days(Days::new(1))
        .expect("yesterday")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{last_run, lookup_purchases, open_work, outbox_count, upsert_purchase};
    use crate::http::{HttpResponse, MapFetcher};
    use crate::ownership::Purchase;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use tempfile::TempDir;

    static ENV: Mutex<()> = Mutex::new(());

    struct TestDb {
        _lock: std::sync::MutexGuard<'static, ()>,
        _dir: TempDir,
        db: WorkDb,
    }

    fn test_db() -> TestDb {
        let lock = ENV.lock().unwrap_or_else(|p| p.into_inner());
        let dir = TempDir::new().unwrap();
        let announce = dir.path().join("announce");
        std::fs::create_dir_all(&announce).unwrap();
        std::env::set_var("STATE_CAPTURE_ANNOUNCE_DIR", announce.to_str().unwrap());
        let db = open_work(&dir.path().join("edgar-form4.sqlite")).unwrap();
        TestDb {
            _lock: lock,
            _dir: dir,
            db,
        }
    }

    fn fixture_fetcher() -> MapFetcher {
        let date = NaiveDate::from_ymd_opt(2026, 9, 11).unwrap();
        let idx = master_index_url(date);
        let mut urls = HashMap::new();
        urls.insert(
            idx,
            HttpResponse {
                status: 200,
                body: include_str!("../fixtures/master.idx").into(),
            },
        );
        urls.insert(
            TICKERS_URL.to_string(),
            HttpResponse {
                status: 200,
                body: include_str!("../fixtures/company_tickers_exchange.json").into(),
            },
        );
        urls.insert(
            filing_url("edgar/data/320193/0000320193-26-000200.txt"),
            HttpResponse {
                status: 200,
                body: include_str!("../fixtures/aapl-form4-purchase.txt").into(),
            },
        );
        urls.insert(
            filing_url("edgar/data/104169/0000104169-26-000060.txt"),
            HttpResponse {
                status: 200,
                body: include_str!("../fixtures/wmt-form4-sale.txt").into(),
            },
        );
        urls.insert(
            filing_url("edgar/data/320193/0000320193-26-000201.txt"),
            HttpResponse {
                status: 200,
                body: include_str!("../fixtures/aapl-form4a-purchase.txt").into(),
            },
        );
        MapFetcher { urls }
    }

    #[test]
    fn weekend_404_is_ok_zero_filings() {
        let mut t = test_db();
        let date = NaiveDate::from_ymd_opt(2026, 9, 12).unwrap();
        let url = master_index_url(date);
        let mut fetcher = MapFetcher {
            urls: HashMap::from([(
                url,
                HttpResponse {
                    status: 404,
                    body: "not found".into(),
                },
            )]),
        };
        let stats = ingest_day(&mut t.db, date, &mut fetcher).unwrap();
        assert_eq!(stats.status, "ok");
        assert_eq!(stats.filings_seen, 0);
        let run = last_run(&t.db).unwrap().unwrap();
        assert_eq!(run.status, "ok");
        assert_eq!(run.as_of_date, "2026-09-12");
    }

    #[test]
    fn weekend_403_is_ok_zero_filings() {
        let mut t = test_db();
        let date = NaiveDate::from_ymd_opt(2026, 9, 12).unwrap();
        let url = master_index_url(date);
        let mut fetcher = MapFetcher {
            urls: HashMap::from([(
                url,
                HttpResponse {
                    status: 403,
                    body: "forbidden".into(),
                },
            )]),
        };
        let stats = ingest_day(&mut t.db, date, &mut fetcher).unwrap();
        assert_eq!(stats.status, "ok");
        assert_eq!(stats.filings_seen, 0);
        let run = last_run(&t.db).unwrap().unwrap();
        assert_eq!(run.status, "ok");
    }

    #[test]
    fn weekday_403_is_error() {
        let mut t = test_db();
        let date = NaiveDate::from_ymd_opt(2026, 9, 11).unwrap();
        let url = master_index_url(date);
        let mut fetcher = MapFetcher {
            urls: HashMap::from([(
                url,
                HttpResponse {
                    status: 403,
                    body: "forbidden".into(),
                },
            )]),
        };
        let err = ingest_day(&mut t.db, date, &mut fetcher).unwrap_err();
        assert!(err.to_string().contains("HTTP 403"));
        let run = last_run(&t.db).unwrap().unwrap();
        assert_eq!(run.status, "error");
        assert_eq!(run.as_of_date, "2026-09-11");
    }

    #[test]
    fn ingest_fixture_and_unchanged_rerun_no_extra_outbox() {
        let mut t = test_db();
        let date = NaiveDate::from_ymd_opt(2026, 9, 11).unwrap();
        let mut fetcher = fixture_fetcher();
        let stats = ingest_day(&mut t.db, date, &mut fetcher).unwrap();
        assert_eq!(stats.filings_seen, 3);
        assert_eq!(stats.txt_ok, 3);
        assert_eq!(stats.filings_failed, 0);
        assert_eq!(stats.filings_upserted, 2);
        assert_eq!(stats.status, "ok");
        let n1 = outbox_count(&t.db).unwrap();
        assert!(n1 >= 3, "2 purchases + 1 run, got {n1}");

        let aapl = lookup_purchases(&t.db, "AAPL").unwrap();
        assert_eq!(aapl.len(), 2);
        assert!(aapl.iter().all(|p| p.source == "txt"));
        assert!(aapl.iter().any(|p| p.form == "4"));
        assert!(aapl.iter().any(|p| p.form == "4/A" && p.is_amendment == 1));
        assert_eq!(aapl[0].ticker.as_deref(), Some("AAPL"));

        let sale = lookup_purchases(&t.db, "WMT").unwrap();
        assert!(sale.is_empty(), "code S must not be stored");

        let mut fetcher = fixture_fetcher();
        ingest_day(&mut t.db, date, &mut fetcher).unwrap();
        let n2 = outbox_count(&t.db).unwrap();
        assert_eq!(n2, n1 + 1, "unchanged purchases must not emit extra outbox");
    }

    #[test]
    fn txt_403_counts_as_failed() {
        let mut t = test_db();
        let date = NaiveDate::from_ymd_opt(2026, 9, 11).unwrap();
        let mut urls = HashMap::new();
        urls.insert(
            master_index_url(date),
            HttpResponse {
                status: 200,
                body: include_str!("../fixtures/master.idx").into(),
            },
        );
        urls.insert(
            TICKERS_URL.to_string(),
            HttpResponse {
                status: 200,
                body: include_str!("../fixtures/company_tickers_exchange.json").into(),
            },
        );
        urls.insert(
            filing_url("edgar/data/320193/0000320193-26-000200.txt"),
            HttpResponse {
                status: 403,
                body: "forbidden".into(),
            },
        );
        urls.insert(
            filing_url("edgar/data/104169/0000104169-26-000060.txt"),
            HttpResponse {
                status: 200,
                body: include_str!("../fixtures/wmt-form4-sale.txt").into(),
            },
        );
        urls.insert(
            filing_url("edgar/data/320193/0000320193-26-000201.txt"),
            HttpResponse {
                status: 200,
                body: include_str!("../fixtures/aapl-form4a-purchase.txt").into(),
            },
        );
        let mut fetcher = MapFetcher { urls };
        let stats = ingest_day(&mut t.db, date, &mut fetcher).unwrap();
        assert_eq!(stats.filings_failed, 1);
        assert_eq!(stats.status, "partial");
        assert_eq!(stats.filings_upserted, 1);
    }

    #[test]
    fn rolled_back_tx_zero_outbox() {
        let t = test_db();
        let before = outbox_count(&t.db).unwrap();
        {
            let tx = t.db.unchecked_transaction().unwrap();
            let p = Purchase {
                trade_id: "x|0000000001|2026-09-11|Common Stock|1||D".into(),
                accession: "0000000000-26-000001".into(),
                cik: "0000000000".into(),
                ticker: None,
                company_name: "X".into(),
                form: "4".into(),
                is_amendment: 0,
                filed_date: "2026-09-11".into(),
                owner_cik: "0000000001".into(),
                rpt_owner_name: "X".into(),
                officer_title: None,
                is_director: 0,
                is_officer: 1,
                transaction_date: "2026-09-11".into(),
                security_title: "Common Stock".into(),
                transaction_shares: "1".into(),
                transaction_price: None,
                acquired_disposed: Some("A".into()),
                direct_or_indirect: Some("D".into()),
                filename: "edgar/data/0/x.txt".into(),
                source: "txt".into(),
            };
            upsert_purchase(&tx, &p).unwrap();
            tx.rollback().unwrap();
        }
        assert_eq!(outbox_count(&t.db).unwrap(), before);
    }

    #[test]
    fn capture_installs_outbox() {
        let t = test_db();
        let n: i64 =
            t.db.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='_outbox'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn source_has_no_insert_or_replace() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        fn walk(p: &std::path::Path, hits: &mut Vec<String>) {
            for e in std::fs::read_dir(p).unwrap() {
                let e = e.unwrap();
                let path = e.path();
                if path.is_dir() {
                    walk(&path, hits);
                    continue;
                }
                if path.extension().and_then(|s| s.to_str()) != Some("rs") {
                    continue;
                }
                let body = std::fs::read_to_string(&path).unwrap();
                let needle = format!("{}{}{}", "OR ", "REPLACE ", "INTO");
                if body.to_ascii_uppercase().contains(&needle) {
                    hits.push(path.display().to_string());
                }
            }
        }
        let mut hits = Vec::new();
        walk(&root, &mut hits);
        assert!(hits.is_empty(), "forbidden replace idiom in {hits:?}");
    }
}
