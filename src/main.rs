use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};
use edgar_form4::db::{last_run, lookup_purchases, open, open_work};
use edgar_form4::http::{validate_sleep, LiveFetcher, DEFAULT_SLEEP_SECS};
use edgar_form4::ingest::{default_as_of, ingest_day, parse_as_of};
use edgar_form4::sec_ua::validate_user_agent;

#[derive(Parser)]
#[command(
    name = "edgar-form4",
    about = "Nightly EDGAR Form 4 / 4/A open-market purchases (code P). Labels, not leads.",
    version
)]
struct Cli {
    #[arg(
        long,
        global = true,
        default_value = "data/edgar-form4.sqlite",
        env = "EDGAR_FORM4_SQLITE"
    )]
    db: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Fetch one UTC day's master index and upsert Form 4 code-P purchases
    Ingest {
        /// UTC calendar day (default: yesterday UTC)
        #[arg(long)]
        date: Option<String>,
        #[arg(long, env = "EDGAR_SLEEP_SECS", default_value_t = DEFAULT_SLEEP_SECS)]
        sleep: f64,
        #[arg(long, env = "SEC_USER_AGENT")]
        sec_user_agent: Option<String>,
    },
    /// Last ingest_runs row
    Status,
    /// Purchases by CIK, ticker, accession, owner CIK, or owner name
    Lookup { query: String },
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    match run() {
        Ok(code) => code,
        Err(err) => {
            tracing::error!("{err:#}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    match cli.command {
        Command::Ingest {
            date,
            sleep,
            sec_user_agent,
        } => {
            validate_sleep(sleep)?;
            let ua = validate_user_agent(sec_user_agent.as_deref().unwrap_or(""))?;
            let day = match date {
                Some(s) => parse_as_of(&s)?,
                None => default_as_of(),
            };
            let mut db = open_work(&cli.db)?;
            let mut fetcher = LiveFetcher::new(&ua, sleep)?;
            let stats = ingest_day(&mut db, day, &mut fetcher)?;
            tracing::info!(
                date = %day,
                status = %stats.status,
                seen = stats.filings_seen,
                upserted = stats.filings_upserted,
                failed = stats.filings_failed,
                txt_ok = stats.txt_ok,
                "ingest finished"
            );
            if stats.status == "error" {
                Ok(ExitCode::from(1))
            } else {
                Ok(ExitCode::SUCCESS)
            }
        }
        Command::Status => {
            let conn = open(&cli.db)?;
            match last_run(&conn)? {
                None => println!("no ingest_runs"),
                Some(r) => {
                    println!(
                        "as_of_date={} status={} seen={} upserted={} failed={} started_at={} finished_at={}",
                        r.as_of_date,
                        r.status,
                        r.filings_seen,
                        r.filings_upserted,
                        r.filings_failed,
                        r.started_at,
                        r.finished_at
                    );
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Lookup { query } => {
            let conn = open(&cli.db)?;
            let rows = lookup_purchases(&conn, &query)?;
            if rows.is_empty() {
                println!("no purchases for {query}");
            } else {
                for p in rows {
                    println!(
                        "{} {} {} {} {} {} shares={} price={} {}",
                        p.transaction_date,
                        p.accession,
                        p.ticker.as_deref().unwrap_or("-"),
                        p.form,
                        p.rpt_owner_name,
                        p.security_title,
                        p.transaction_shares,
                        p.transaction_price.as_deref().unwrap_or("-"),
                        p.company_name
                    );
                }
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}
