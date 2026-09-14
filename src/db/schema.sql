CREATE TABLE IF NOT EXISTS purchases (
    trade_id TEXT PRIMARY KEY,
    accession TEXT NOT NULL,
    cik TEXT NOT NULL,
    ticker TEXT,
    company_name TEXT NOT NULL,
    form TEXT NOT NULL,
    is_amendment INTEGER NOT NULL CHECK (is_amendment IN (0, 1)),
    filed_date TEXT NOT NULL,
    owner_cik TEXT NOT NULL,
    rpt_owner_name TEXT NOT NULL,
    officer_title TEXT,
    is_director INTEGER NOT NULL CHECK (is_director IN (0, 1)),
    is_officer INTEGER NOT NULL CHECK (is_officer IN (0, 1)),
    transaction_date TEXT NOT NULL,
    security_title TEXT NOT NULL,
    transaction_shares TEXT NOT NULL,
    transaction_price TEXT,
    acquired_disposed TEXT,
    direct_or_indirect TEXT,
    filename TEXT NOT NULL,
    source TEXT NOT NULL CHECK (source IN ('txt')),
    deleted_at INTEGER
) STRICT;

CREATE TABLE IF NOT EXISTS ingest_runs (
    as_of_date TEXT PRIMARY KEY,
    started_at TEXT NOT NULL,
    finished_at TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('ok', 'partial', 'error')),
    index_url TEXT NOT NULL,
    filings_seen INTEGER NOT NULL,
    filings_upserted INTEGER NOT NULL,
    filings_failed INTEGER NOT NULL,
    txt_ok INTEGER NOT NULL
) STRICT;
