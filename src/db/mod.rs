use std::ops::{Deref, DerefMut};
use std::path::Path;

use anyhow::{Context, Result};
use capturable_state::{
    apply_runtime_pragmas, install, CaptureConfig, CaptureMode, Nudge, TableSpec,
};
use rusqlite::{params, Connection, OptionalExtension};

use crate::ownership::Purchase;
use crate::time::utc_iso;

pub const DB_NAME: &str = "edgar-form4";

pub struct WorkDb {
    conn: Connection,
    pub nudge: Nudge,
}

impl Deref for WorkDb {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.conn
    }
}

impl DerefMut for WorkDb {
    fn deref_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }
}

fn open_conn(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
    }
    let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
    apply_runtime_pragmas(&conn)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    apply_schema(&conn)?;
    Ok(conn)
}

pub fn open(path: &Path) -> Result<Connection> {
    open_conn(path)
}

pub fn open_work(path: &Path) -> Result<WorkDb> {
    let conn = open_conn(path)?;
    let nudge = install_capture(&conn, path)?;
    Ok(WorkDb { conn, nudge })
}

fn apply_schema(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.execute_batch(include_str!("schema.sql"))
        .context("apply schema")?;
    Ok(())
}

fn install_capture(conn: &Connection, path: &Path) -> Result<Nudge> {
    let tables = [
        TableSpec::new("purchases", CaptureMode::After),
        TableSpec::new("ingest_runs", CaptureMode::After),
    ];
    install(conn, &CaptureConfig::new(DB_NAME, path, &tables))
}

/// Change-aware upsert. Identical reruns emit no extra `_outbox` row.
pub fn upsert_purchase(conn: &Connection, p: &Purchase) -> Result<bool> {
    let n = conn.execute(
        "INSERT INTO purchases (
            trade_id, accession, cik, ticker, company_name, form, is_amendment,
            filed_date, owner_cik, rpt_owner_name, officer_title, is_director,
            is_officer, transaction_date, security_title, transaction_shares,
            transaction_price, acquired_disposed, direct_or_indirect,
            filename, source, deleted_at
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
            ?16, ?17, ?18, ?19, ?20, ?21, NULL
         )
         ON CONFLICT(trade_id) DO UPDATE SET
            accession = excluded.accession,
            cik = excluded.cik,
            ticker = excluded.ticker,
            company_name = excluded.company_name,
            form = excluded.form,
            is_amendment = excluded.is_amendment,
            filed_date = excluded.filed_date,
            owner_cik = excluded.owner_cik,
            rpt_owner_name = excluded.rpt_owner_name,
            officer_title = excluded.officer_title,
            is_director = excluded.is_director,
            is_officer = excluded.is_officer,
            transaction_date = excluded.transaction_date,
            security_title = excluded.security_title,
            transaction_shares = excluded.transaction_shares,
            transaction_price = excluded.transaction_price,
            acquired_disposed = excluded.acquired_disposed,
            direct_or_indirect = excluded.direct_or_indirect,
            filename = excluded.filename,
            source = excluded.source
         WHERE purchases.accession IS NOT excluded.accession
            OR purchases.cik IS NOT excluded.cik
            OR purchases.ticker IS NOT excluded.ticker
            OR purchases.company_name IS NOT excluded.company_name
            OR purchases.form IS NOT excluded.form
            OR purchases.is_amendment IS NOT excluded.is_amendment
            OR purchases.filed_date IS NOT excluded.filed_date
            OR purchases.owner_cik IS NOT excluded.owner_cik
            OR purchases.rpt_owner_name IS NOT excluded.rpt_owner_name
            OR purchases.officer_title IS NOT excluded.officer_title
            OR purchases.is_director IS NOT excluded.is_director
            OR purchases.is_officer IS NOT excluded.is_officer
            OR purchases.transaction_date IS NOT excluded.transaction_date
            OR purchases.security_title IS NOT excluded.security_title
            OR purchases.transaction_shares IS NOT excluded.transaction_shares
            OR purchases.transaction_price IS NOT excluded.transaction_price
            OR purchases.acquired_disposed IS NOT excluded.acquired_disposed
            OR purchases.direct_or_indirect IS NOT excluded.direct_or_indirect
            OR purchases.filename IS NOT excluded.filename
            OR purchases.source IS NOT excluded.source",
        params![
            p.trade_id,
            p.accession,
            p.cik,
            p.ticker.as_deref(),
            p.company_name,
            p.form,
            p.is_amendment,
            p.filed_date,
            p.owner_cik,
            p.rpt_owner_name,
            p.officer_title.as_deref(),
            p.is_director,
            p.is_officer,
            p.transaction_date,
            p.security_title,
            p.transaction_shares,
            p.transaction_price.as_deref(),
            p.acquired_disposed.as_deref(),
            p.direct_or_indirect.as_deref(),
            p.filename,
            p.source,
        ],
    )?;
    Ok(n > 0)
}

#[allow(clippy::too_many_arguments)]
pub fn upsert_run(
    conn: &Connection,
    as_of_date: &str,
    started_at: chrono::DateTime<chrono::Utc>,
    finished_at: chrono::DateTime<chrono::Utc>,
    status: &str,
    index_url: &str,
    seen: i64,
    upserted: i64,
    failed: i64,
    txt_ok: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO ingest_runs (
            as_of_date, started_at, finished_at, status, index_url,
            filings_seen, filings_upserted, filings_failed, txt_ok
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(as_of_date) DO UPDATE SET
            started_at = excluded.started_at,
            finished_at = excluded.finished_at,
            status = excluded.status,
            index_url = excluded.index_url,
            filings_seen = excluded.filings_seen,
            filings_upserted = excluded.filings_upserted,
            filings_failed = excluded.filings_failed,
            txt_ok = excluded.txt_ok",
        params![
            as_of_date,
            utc_iso(started_at),
            utc_iso(finished_at),
            status,
            index_url,
            seen,
            upserted,
            failed,
            txt_ok,
        ],
    )?;
    Ok(())
}

#[derive(Debug)]
pub struct StatusRow {
    pub as_of_date: String,
    pub started_at: String,
    pub finished_at: String,
    pub status: String,
    pub filings_seen: i64,
    pub filings_upserted: i64,
    pub filings_failed: i64,
}

pub fn last_run(conn: &Connection) -> Result<Option<StatusRow>> {
    conn.query_row(
        "SELECT as_of_date, started_at, finished_at, status,
                filings_seen, filings_upserted, filings_failed
         FROM ingest_runs
         ORDER BY as_of_date DESC
         LIMIT 1",
        [],
        |r| {
            Ok(StatusRow {
                as_of_date: r.get(0)?,
                started_at: r.get(1)?,
                finished_at: r.get(2)?,
                status: r.get(3)?,
                filings_seen: r.get(4)?,
                filings_upserted: r.get(5)?,
                filings_failed: r.get(6)?,
            })
        },
    )
    .optional()
    .context("last ingest_runs")
}

pub fn lookup_purchases(conn: &Connection, q: &str) -> Result<Vec<Purchase>> {
    let q = q.trim();
    let cik = crate::tickers::pad_cik(q);
    let acc = crate::index::normalize_accession(q);
    let ticker = q.to_ascii_uppercase();
    let mut stmt = conn.prepare(
        "SELECT trade_id, accession, cik, ticker, company_name, form, is_amendment,
                filed_date, owner_cik, rpt_owner_name, officer_title, is_director,
                is_officer, transaction_date, security_title, transaction_shares,
                transaction_price, acquired_disposed, direct_or_indirect,
                filename, source
         FROM purchases
         WHERE accession = ?1 OR cik = ?2 OR ticker = ?3 OR owner_cik = ?2
               OR rpt_owner_name LIKE ?4 ESCAPE '\\'
         ORDER BY transaction_date DESC, accession, trade_id",
    )?;
    let like = like_substring(q);
    let rows = stmt.query_map(params![acc, cik, ticker, like], |r| {
        Ok(Purchase {
            trade_id: r.get(0)?,
            accession: r.get(1)?,
            cik: r.get(2)?,
            ticker: r.get(3)?,
            company_name: r.get(4)?,
            form: r.get(5)?,
            is_amendment: r.get(6)?,
            filed_date: r.get(7)?,
            owner_cik: r.get(8)?,
            rpt_owner_name: r.get(9)?,
            officer_title: r.get(10)?,
            is_director: r.get(11)?,
            is_officer: r.get(12)?,
            transaction_date: r.get(13)?,
            security_title: r.get(14)?,
            transaction_shares: r.get(15)?,
            transaction_price: r.get(16)?,
            acquired_disposed: r.get(17)?,
            direct_or_indirect: r.get(18)?,
            filename: r.get(19)?,
            source: r.get(20)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn outbox_count(conn: &Connection) -> Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM _outbox", [], |r| r.get(0))
        .context("outbox count")
}

fn like_substring(q: &str) -> String {
    let mut out = String::from("%");
    for c in q.chars() {
        match c {
            '%' | '_' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out.push('%');
    out
}
