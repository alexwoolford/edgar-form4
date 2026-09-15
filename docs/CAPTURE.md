# Capture contract (work sqlite)

Decision: **capture Form 4 / 4/A non-derivative open-market purchases (transaction code P), not filing bodies and not the daily EDGAR tar.gz feed.** Work sqlite is `{--db}` (prod `/var/lib/edgar-form4/edgar-form4.sqlite`). Logical name `edgar-form4`. There is no published `current/` copy. The collector watches work sqlite only.

Canonical contract: [capturable-state design principles](https://github.com/alexwoolford/capturable-state/blob/main/docs/design-principles.md) §0 / §7 and [datetime.md](https://github.com/alexwoolford/capturable-state/blob/main/docs/datetime.md). Capture the trickle, not the hose.

Pin: `capturable-state` git tag `v0.1.1` (not a path dep; do not copy `src/*.rs`).

## What is captured

| Table / stream | Capture? | Mode | Why |
| --- | --- | --- | --- |
| `purchases` | **yes** | after | Product. Key `trade_id` |
| `ingest_runs` | **yes** | after | Did last night finish? |
| HTTP cache / tickers JSON | **no** | — | In-memory per ingest day |
| Filing `.txt` bodies | **no** | — | Hose-adjacent |
| Sales (`S`), gifts, `M`/`F`/`A`, derivative table | **no** | — | Not the capture set |
| `_outbox` | platform | — | Generated |

Identity: `trade_id` = `accession|owner_cik|tx_index|transaction_date|security_title|shares|price|direct_or_indirect`. `tx_index` is the 0-based `nonDerivativeTransaction` position in the filing (all codes, not only P) so two same-day lots with the same size and price stay distinct. Purchases are insert-once; an identical rerun must not emit a new outbox row (`ON CONFLICT … WHERE` any column differs). A 4/A is a **new** accession. Soft-delete unused in v1.

This identity is not compatible with rows written before `tx_index` was added. After upgrade, a rerun inserts new keys; delete the old work sqlite (or accept duplicate labels) rather than mixing both shapes.

A filing with a parseable `<ownershipDocument>` and no code-P rows is a successful skip (not a stored empty row, not a failure). HTTP 200 with no `<ownershipDocument>` is `filings_failed` — do not treat that as “no purchases.” HTTP 200 with no `<reportingOwner>` is also `filings_failed`. A code-P row missing date, title, or shares is logged and skipped (not stored, not a filing failure).

Ticker prefers `<issuerTradingSymbol>` in the ownership XML. `company_tickers_exchange.json` primary-common rank is fetched only as fallback when that tag is missing. Unlisted CIK → ticker NULL.

`filings_upserted` counts **purchase rows**, not filings. A joint filing with two owners and one code-P lot increments it by 2.

Joint filings: one captured row per `(reportingOwner × code-P transaction)`. Cluster pictures count distinct `rpt_owner_name`. Do not sum `transaction_shares` across owners on the same accession — that double-counts a jointly reported lot. `purchases` is not a unique-lot table.

These are **labels**, not leads. Code P is the disclosure, not a mosaic-generated cluster score.

Do not `collect --snapshot` this database.

`--from` / `--to` loops the same per-day upsert (resume-safe). A missing daily `master.YYYYMMDD.idx` is 404, or 403 on Sat/Sun from this OCI IP. Both are an empty successful ingest, not a UA failure. Weekday index 403 is an error (including US holidays — see [DAILY_OPS.md](DAILY_OPS.md)) and stops a range. Filing `.txt` **403/404 are not retried**; they increment `filings_failed`. Transport failures and HTTP 429/5xx retry up to four attempts, then fail that request. Do not guess a purchase.

## Clocks

| Layer | Columns | Type |
| --- | --- | --- |
| Facts | `filed_date`, `transaction_date` | TEXT `YYYY-MM-DD` |
| Facts | run `started_at` / `finished_at` | TEXT `YYYY-MM-DDTHH:MM:SSZ` |
| Envelope | `_outbox.ts`, `deleted_at` | INTEGER Unix seconds |

## Announce / nudge

`install()` on work sqlite. `ReadWritePaths` include `/var/lib/state-capture/announce` (required when the collector is present) and `-/run/state`. `form4` must be in group `state-capture`. Collector host inventory lives in mosaic `deploy/ct-firehose/`, not in this crate.
