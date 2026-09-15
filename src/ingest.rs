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
    let mut tickers: Option<HashMap<String, String>> = None;

    for row in &rows {
        match resolve_filing(fetcher, row, &mut tickers, &mut stats) {
            Ok(purchases) => {
                if let Err(err) = persist_filing(db, &purchases, &mut stats) {
                    stats.status = "error".into();
                    if let Err(run_err) = finish(db, &as_of, started, &stats) {
                        tracing::error!(
                            error = %run_err,
                            "failed to record ingest_runs after persist error"
                        );
                    }
                    return Err(err).context("persist purchases");
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
    finish(db, &as_of, started, &stats)?;
    Ok(stats)
}

fn persist_filing(
    db: &mut WorkDb,
    purchases: &[crate::ownership::Purchase],
    stats: &mut IngestStats,
) -> Result<()> {
    let tx = db.unchecked_transaction()?;
    for p in purchases {
        if upsert_purchase(&tx, p)? {
            stats.filings_upserted += 1;
        }
    }
    tx.commit()?;
    Ok(())
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

fn ensure_tickers<'a>(
    fetcher: &mut dyn Fetcher,
    cache: &'a mut Option<HashMap<String, String>>,
) -> Result<&'a HashMap<String, String>> {
    if cache.is_none() {
        *cache = Some(load_tickers(fetcher)?);
    }
    Ok(cache.as_ref().expect("tickers cache just set"))
}

fn resolve_filing(
    fetcher: &mut dyn Fetcher,
    row: &IndexRow,
    tickers: &mut Option<HashMap<String, String>>,
    stats: &mut IngestStats,
) -> Result<Vec<crate::ownership::Purchase>> {
    let accession = accession_from_filename(&row.filename);
    let url = filing_url(&row.filename);
    let txt = fetcher.get(&url)?;
    if txt.status != 200 {
        anyhow::bail!("filing HTTP {} for {url}", txt.status);
    }
    let mut purchases = parse_purchases(
        &txt.body,
        &accession,
        &row.cik,
        &row.company_name,
        &row.form,
        &row.filed_date,
        &row.filename,
        None,
    )?;
    if purchases.iter().any(|p| p.ticker.is_none()) {
        match ensure_tickers(fetcher, tickers) {
            Ok(map) => {
                let fallback = map.get(&row.cik).cloned();
                for p in &mut purchases {
                    if p.ticker.is_none() {
                        p.ticker = fallback.clone();
                    }
                }
            }
            Err(err) => {
                tracing::warn!(
                    cik = %row.cik,
                    filename = %row.filename,
                    error = %err,
                    "tickers fallback failed; storing NULL ticker"
                );
            }
        }
    }
    stats.txt_ok += 1;
    Ok(purchases)
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

/// Inclusive UTC calendar days `[from, to]`.
pub fn inclusive_days(from: NaiveDate, to: NaiveDate) -> Result<Vec<NaiveDate>> {
    if from > to {
        anyhow::bail!("--from {from} is after --to {to}");
    }
    let mut days = Vec::new();
    let mut d = from;
    loop {
        days.push(d);
        if d == to {
            return Ok(days);
        }
        d = d.succ_opt().context("date overflow")?;
    }
}

/// CLI window: default yesterday, one `--date`, or `--from`/`--to` together.
pub fn ingest_dates(
    date: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<Vec<NaiveDate>> {
    match (date, from, to) {
        (None, None, None) => Ok(vec![default_as_of()]),
        (Some(d), None, None) => Ok(vec![parse_as_of(d)?]),
        (None, Some(f), Some(t)) => inclusive_days(parse_as_of(f)?, parse_as_of(t)?),
        (Some(_), _, _) => anyhow::bail!("--date cannot be combined with --from/--to"),
        _ => anyhow::bail!("--from and --to must both be set"),
    }
}

/// Same-day upsert per UTC calendar day. Stops on weekday index 403 / transport error.
pub fn ingest_range(
    db: &mut WorkDb,
    from: NaiveDate,
    to: NaiveDate,
    fetcher: &mut dyn Fetcher,
) -> Result<Vec<IngestStats>> {
    let mut out = Vec::new();
    for day in inclusive_days(from, to)? {
        tracing::info!(date = %day, "ingest day");
        out.push(ingest_day(db, day, fetcher)?);
    }
    Ok(out)
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
    fn missing_ownership_xml_counts_as_failed() {
        let mut t = test_db();
        let date = NaiveDate::from_ymd_opt(2026, 9, 11).unwrap();
        let filename = "edgar/data/320193/0000320193-26-000299.txt";
        let idx = "Description: fixture\n\nCIK|Company Name|Form Type|Date Filed|Filename\n--------------------------------------------------------------------------------\n0000320193|Apple Inc.|4|20260911|edgar/data/320193/0000320193-26-000299.txt\n";
        let mut urls = HashMap::new();
        urls.insert(
            master_index_url(date),
            HttpResponse {
                status: 200,
                body: idx.into(),
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
            filing_url(filename),
            HttpResponse {
                status: 200,
                body: include_str!("../fixtures/aapl-form4-no-xml.txt").into(),
            },
        );
        let mut fetcher = MapFetcher { urls };
        let stats = ingest_day(&mut t.db, date, &mut fetcher).unwrap();
        assert_eq!(stats.filings_seen, 1);
        assert_eq!(stats.txt_ok, 0);
        assert_eq!(stats.filings_failed, 1);
        assert_eq!(stats.filings_upserted, 0);
        assert_eq!(stats.status, "partial");
        assert!(lookup_purchases(&t.db, "AAPL").unwrap().is_empty());
    }

    #[test]
    fn joint_filing_upserts_one_row_per_owner() {
        let mut t = test_db();
        let date = NaiveDate::from_ymd_opt(2026, 9, 11).unwrap();
        let filename = "edgar/data/320193/0000320193-26-000210.txt";
        let idx = "Description: fixture\n\nCIK|Company Name|Form Type|Date Filed|Filename\n--------------------------------------------------------------------------------\n0000320193|Apple Inc.|4|20260911|edgar/data/320193/0000320193-26-000210.txt\n";
        let mut urls = HashMap::new();
        urls.insert(
            master_index_url(date),
            HttpResponse {
                status: 200,
                body: idx.into(),
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
            filing_url(filename),
            HttpResponse {
                status: 200,
                body: include_str!("../fixtures/aapl-form4-joint.txt").into(),
            },
        );
        let mut fetcher = MapFetcher { urls };
        let stats = ingest_day(&mut t.db, date, &mut fetcher).unwrap();
        assert_eq!(stats.filings_seen, 1);
        assert_eq!(stats.txt_ok, 1);
        assert_eq!(stats.filings_failed, 0);
        assert_eq!(stats.filings_upserted, 2);
        assert_eq!(stats.status, "ok");
        let rows = lookup_purchases(&t.db, "AAPL").unwrap();
        assert_eq!(rows.len(), 2);
        let names: Vec<&str> = rows.iter().map(|r| r.rpt_owner_name.as_str()).collect();
        assert!(names.contains(&"Cook Timothy D"));
        assert!(names.contains(&"Maestri Luca"));
    }

    #[test]
    fn rolled_back_tx_zero_outbox() {
        let t = test_db();
        let before = outbox_count(&t.db).unwrap();
        {
            let tx = t.db.unchecked_transaction().unwrap();
            let p = Purchase {
                trade_id: "x|0000000001|0|2026-09-11|Common Stock|1||D".into(),
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
    fn xml_ticker_does_not_require_tickers_json() {
        let mut t = test_db();
        let date = NaiveDate::from_ymd_opt(2026, 9, 11).unwrap();
        let filename = "edgar/data/320193/0000320193-26-000200.txt";
        let idx = "Description: fixture\n\nCIK|Company Name|Form Type|Date Filed|Filename\n--------------------------------------------------------------------------------\n0000320193|Apple Inc.|4|20260911|edgar/data/320193/0000320193-26-000200.txt\n";
        let mut urls = HashMap::new();
        urls.insert(
            master_index_url(date),
            HttpResponse {
                status: 200,
                body: idx.into(),
            },
        );
        urls.insert(
            filing_url(filename),
            HttpResponse {
                status: 200,
                body: include_str!("../fixtures/aapl-form4-purchase.txt").into(),
            },
        );
        let mut fetcher = MapFetcher { urls };
        let stats = ingest_day(&mut t.db, date, &mut fetcher).unwrap();
        assert_eq!(stats.status, "ok");
        assert_eq!(stats.filings_upserted, 1);
        let rows = lookup_purchases(&t.db, "AAPL").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].ticker.as_deref(), Some("AAPL"));
    }

    #[test]
    fn two_identical_p_rows_upsert_separately() {
        let mut t = test_db();
        let date = NaiveDate::from_ymd_opt(2026, 9, 11).unwrap();
        let filename = "edgar/data/320193/0000320193-26-000220.txt";
        let idx = "Description: fixture\n\nCIK|Company Name|Form Type|Date Filed|Filename\n--------------------------------------------------------------------------------\n0000320193|Apple Inc.|4|20260911|edgar/data/320193/0000320193-26-000220.txt\n";
        let mut urls = HashMap::new();
        urls.insert(
            master_index_url(date),
            HttpResponse {
                status: 200,
                body: idx.into(),
            },
        );
        urls.insert(
            filing_url(filename),
            HttpResponse {
                status: 200,
                body: include_str!("../fixtures/aapl-form4-two-purchases.txt").into(),
            },
        );
        let mut fetcher = MapFetcher { urls };
        let stats = ingest_day(&mut t.db, date, &mut fetcher).unwrap();
        assert_eq!(stats.filings_upserted, 2);
        assert_eq!(stats.status, "ok");
        let rows = lookup_purchases(&t.db, "AAPL").unwrap();
        assert_eq!(rows.len(), 2);
        assert_ne!(rows[0].trade_id, rows[1].trade_id);
    }

    #[test]
    fn missing_reporting_owner_counts_as_failed() {
        let mut t = test_db();
        let date = NaiveDate::from_ymd_opt(2026, 9, 11).unwrap();
        let filename = "edgar/data/320193/0000320193-26-000298.txt";
        let idx = "Description: fixture\n\nCIK|Company Name|Form Type|Date Filed|Filename\n--------------------------------------------------------------------------------\n0000320193|Apple Inc.|4|20260911|edgar/data/320193/0000320193-26-000298.txt\n";
        let body = r#"<ownershipDocument>
  <issuerTradingSymbol>AAPL</issuerTradingSymbol>
  <nonDerivativeTable>
    <nonDerivativeTransaction>
      <securityTitle><value>Common Stock</value></securityTitle>
      <transactionDate><value>2026-09-10</value></transactionDate>
      <transactionCoding><transactionCode>P</transactionCode></transactionCoding>
      <transactionShares><value>1</value></transactionShares>
    </nonDerivativeTransaction>
  </nonDerivativeTable>
</ownershipDocument>"#;
        let mut urls = HashMap::new();
        urls.insert(
            master_index_url(date),
            HttpResponse {
                status: 200,
                body: idx.into(),
            },
        );
        urls.insert(
            filing_url(filename),
            HttpResponse {
                status: 200,
                body: body.into(),
            },
        );
        let mut fetcher = MapFetcher { urls };
        let stats = ingest_day(&mut t.db, date, &mut fetcher).unwrap();
        assert_eq!(stats.filings_failed, 1);
        assert_eq!(stats.status, "partial");
        assert!(lookup_purchases(&t.db, "AAPL").unwrap().is_empty());
    }

    #[test]
    fn lookup_like_wildcards_are_literal() {
        let mut t = test_db();
        let date = NaiveDate::from_ymd_opt(2026, 9, 11).unwrap();
        let mut fetcher = fixture_fetcher();
        ingest_day(&mut t.db, date, &mut fetcher).unwrap();
        assert!(lookup_purchases(&t.db, "%").unwrap().is_empty());
        assert!(lookup_purchases(&t.db, "_").unwrap().is_empty());
        assert!(!lookup_purchases(&t.db, "Cook").unwrap().is_empty());
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

    #[test]
    fn ingest_dates_from_to_inclusive() {
        let days = ingest_dates(None, Some("2026-06-08"), Some("2026-06-10")).unwrap();
        assert_eq!(
            days,
            vec![
                NaiveDate::from_ymd_opt(2026, 6, 8).unwrap(),
                NaiveDate::from_ymd_opt(2026, 6, 9).unwrap(),
                NaiveDate::from_ymd_opt(2026, 6, 10).unwrap(),
            ]
        );
    }

    #[test]
    fn ingest_dates_rejects_from_after_to() {
        let err = ingest_dates(None, Some("2026-06-10"), Some("2026-06-08")).unwrap_err();
        assert!(err.to_string().contains("after"));
    }

    #[test]
    fn ingest_range_weekend_then_weekday() {
        let mut t = test_db();
        let fri = NaiveDate::from_ymd_opt(2026, 9, 11).unwrap();
        let sat = NaiveDate::from_ymd_opt(2026, 9, 12).unwrap();
        let mut fetcher = fixture_fetcher();
        fetcher.urls.insert(
            master_index_url(sat),
            HttpResponse {
                status: 404,
                body: "not found".into(),
            },
        );
        let stats = ingest_range(&mut t.db, fri, sat, &mut fetcher).unwrap();
        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0].status, "ok");
        assert!(stats[0].filings_upserted >= 1);
        assert_eq!(stats[1].status, "ok");
        assert_eq!(stats[1].filings_seen, 0);
        assert!(!lookup_purchases(&t.db, "AAPL").unwrap().is_empty());
    }

    #[test]
    fn ingest_range_stops_on_weekday_403() {
        let mut t = test_db();
        let fri = NaiveDate::from_ymd_opt(2026, 9, 11).unwrap();
        let sat = NaiveDate::from_ymd_opt(2026, 9, 12).unwrap();
        let mut fetcher = MapFetcher {
            urls: HashMap::from([
                (
                    master_index_url(fri),
                    HttpResponse {
                        status: 403,
                        body: "forbidden".into(),
                    },
                ),
                (
                    master_index_url(sat),
                    HttpResponse {
                        status: 404,
                        body: "not found".into(),
                    },
                ),
            ]),
        };
        let err = ingest_range(&mut t.db, fri, sat, &mut fetcher).unwrap_err();
        assert!(err.to_string().contains("HTTP 403"));
        let run = last_run(&t.db).unwrap().unwrap();
        assert_eq!(run.as_of_date, "2026-09-11");
        assert_eq!(run.status, "error");
    }
}
