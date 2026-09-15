# Daily ops (Oracle)

Oneshot + timer. The OS is the scheduler. Do not add an in-process cron.

Operator logs: `tracing` on stderr → journald (`SyslogIdentifier=edgar-form4-ingest`). Default `RUST_LOG=info`.

`ingest_runs` is capturable domain telemetry. Query it in mosaic.

## Layout

| | |
| --- | --- |
| Prefix | `/opt/edgar-form4` |
| State | `/var/lib/edgar-form4/edgar-form4.sqlite` |
| User | `form4` (do not reuse `edgar`) |
| Env | `/opt/edgar-form4/etc/edgar-form4.env` (`chmod 600`) |
| Timer | `edgar-form4-ingest.timer` **08:00 UTC** + 15m jitter, `Persistent=true` |
| Oneshot timeout | `TimeoutStartSec=3h` (Form 4 is the largest daily EDGAR form type; hundreds of `.txt` GETs at 0.5s sleep, peak days can exceed 1h) |

No published `current/`. Do not copy a laptop sqlite onto the host.

## Install

```bash
cargo build --release
sudo ./deploy/install.sh
# set SEC_USER_AGENT in /opt/edgar-form4/etc/edgar-form4.env
```

`install.sh` enables the timer **without** `--now`. First run: `sudo systemctl start edgar-form4-ingest.service`.

## Timer failed

1. `systemctl list-failed --no-pager`
2. `journalctl -u edgar-form4-ingest.service -n 80 --no-pager`
3. `edgar-form4 --db /var/lib/edgar-form4/edgar-form4.sqlite status`
4. Re-run: `sudo systemctl start edgar-form4-ingest.service`

Do not hand-edit sqlite.

Weekend / US holiday master-index **404 is success** (`status=ok`, zero filings). From this OCI IP an unpublished weekend path is often **403** rather than 404 — Sat/Sun 403 is the same success. Weekday index 403 is `status=error` (UA/Akamai), **including weekday US holidays**. If the NYSE calendar was closed and the index 403'd, treat it as a closed market: do not hand-edit sqlite; the next session's ingest is the next real day. Index 5xx and transport errors retry (four attempts, exponential backoff) then `status=error` (unit failed).

Some filings 403/404 (not retried), or HTTP 200 with no `<ownershipDocument>` / no `<reportingOwner>`, is `partial` (**exit 1** — systemd marks the oneshot failed). Query `ingest_runs.filings_failed` in mosaic. `filings_upserted` is purchase rows, not filing count. Do not lower `EDGAR_SLEEP_SECS` to beat the 3h timeout (SEC 10 req/s ceiling).

## Backfill (`--from` / `--to`)

Same per-day function as the timer. Form 4 `transaction_date` already spans older trades inside one filing day; 8-K `filed_date` is the scarce side. Run a range **after** 8-K history exists. Do not raise nightly `TimeoutStartSec` for this — Form 4 is heavier than 8-K (more `.txt` GETs). Invoke `run-ingest.sh` with `EDGAR_INGEST_FROM` / `EDGAR_INGEST_TO` outside the timer. Weekday 403 stops the loop.

## SEC fair access

Official: [Accessing EDGAR Data](https://www.sec.gov/search-filings/edgar-search-assistance/accessing-edgar-data).

- `SEC_USER_AGENT` sample shape: `edgar-form4 you@real-domain`
- Refuse `example.com` and github-paren UAs (Akamai 403 undeclared bot)
- Default sleep 0.5s (~2 req/s). Ceiling 10 req/s. Do not rotate User-Agents (cap is per IP)
- Two 403s: undeclared-bot UA vs datacenter IP reputation. This host already GETs `company_tickers_exchange.json` for 8-K labels. Filing `.txt` 403/404 is a failed filing, not a guessed purchase.

Do not send `FAA_USER_AGENT` to SEC. Do not download `Feed/*.nc.tar.gz`.
