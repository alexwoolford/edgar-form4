//! SEC `company_tickers_exchange.json` → one primary common ticker per CIK.
//! Ranking copied from tail-to-ticker (no path dep).

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Company {
    pub cik: String,
    pub ticker: String,
    pub name: String,
    pub exchange: String,
}

pub fn pad_cik(cik: &str) -> String {
    let digits: String = cik.chars().filter(|c| c.is_ascii_digit()).collect();
    format!("{digits:0>10}")
}

#[derive(Debug, Deserialize)]
struct TickersFile {
    fields: Vec<String>,
    data: Vec<Vec<Value>>,
}

pub fn parse_tickers_json(body: &[u8]) -> Result<Vec<Company>> {
    if let Ok(file) = serde_json::from_slice::<TickersFile>(body) {
        return Ok(from_columnar(file));
    }
    let v: Value = serde_json::from_slice(body).context("tickers JSON")?;
    if let Some(obj) = v.as_object() {
        let mut out = Vec::new();
        for (_k, row) in obj {
            if let Some(c) = from_object_row(row) {
                out.push(c);
            }
        }
        if !out.is_empty() {
            return Ok(out);
        }
    }
    anyhow::bail!("unrecognized company_tickers JSON shape")
}

fn from_columnar(file: TickersFile) -> Vec<Company> {
    let idx: HashMap<String, usize> = file
        .fields
        .iter()
        .enumerate()
        .map(|(i, f)| (f.to_ascii_lowercase(), i))
        .collect();
    let get = |row: &[Value], key: &str| -> String {
        idx.get(key)
            .and_then(|i| row.get(*i))
            .map(value_to_string)
            .unwrap_or_default()
    };
    file.data
        .into_iter()
        .filter_map(|row| {
            let ticker = get(&row, "ticker");
            if ticker.is_empty() {
                return None;
            }
            Some(Company {
                cik: pad_cik(&get(&row, "cik")),
                ticker: ticker.to_uppercase(),
                name: get(&row, "name"),
                exchange: get(&row, "exchange"),
            })
        })
        .collect()
}

fn from_object_row(row: &Value) -> Option<Company> {
    let ticker = row
        .get("ticker")
        .or_else(|| row.get("tickers"))
        .map(value_to_string)
        .filter(|s| !s.is_empty())?;
    let cik = row
        .get("cik")
        .or_else(|| row.get("cik_str"))
        .map(value_to_string)
        .unwrap_or_default();
    let name = row
        .get("name")
        .or_else(|| row.get("title"))
        .map(value_to_string)
        .unwrap_or_default();
    Some(Company {
        cik: pad_cik(&cik),
        ticker: ticker.to_uppercase(),
        name,
        exchange: row.get("exchange").map(value_to_string).unwrap_or_default(),
    })
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

/// One ticker per CIK, preferring a common share over preferreds/warrants.
pub fn primary_listings(companies: &[Company]) -> HashMap<String, String> {
    let mut best: HashMap<String, Company> = HashMap::new();
    for c in companies {
        match best.get(&c.cik) {
            Some(cur) if common_share_rank(c) <= common_share_rank(cur) => {}
            _ => {
                best.insert(c.cik.clone(), c.clone());
            }
        }
    }
    best.into_iter().map(|(cik, c)| (cik, c.ticker)).collect()
}

fn major_exchange(exchange: &str) -> bool {
    matches!(
        exchange.trim().to_ascii_uppercase().as_str(),
        "NYSE" | "NASDAQ" | "NYSE ARCA" | "NYSE AMERICAN" | "NYSE MKT" | "AMEX"
    )
}

fn structured_preferred_or_warrant(ticker: &str) -> bool {
    let t = ticker.trim().to_ascii_uppercase();
    if let Some((_, rest)) = t.split_once('-') {
        return rest.starts_with('P') || rest.starts_with('W');
    }
    false
}

fn series_suffix_unhyphenated(ticker: &str) -> bool {
    let t = ticker.trim().to_ascii_uppercase();
    if t.contains('-') || t.contains('.') {
        return false;
    }
    t.len() >= 5 && matches!(t.chars().last(), Some('P' | 'O' | 'W'))
}

fn common_share_rank(c: &Company) -> (u8, u8, u8, i32, std::cmp::Reverse<String>) {
    let t = c.ticker.trim().to_ascii_uppercase();
    let hyphen = t.contains('-') || t.contains('.');
    (
        u8::from(!hyphen),
        u8::from(major_exchange(&c.exchange)),
        u8::from(!(structured_preferred_or_warrant(&t) || series_suffix_unhyphenated(&t))),
        -(t.len() as i32),
        std::cmp::Reverse(t),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn co(cik: &str, ticker: &str, exchange: &str) -> Company {
        Company {
            cik: pad_cik(cik),
            ticker: ticker.into(),
            name: "X".into(),
            exchange: exchange.into(),
        }
    }

    #[test]
    fn parses_columnar_tickers() {
        let json = include_bytes!("../fixtures/company_tickers_exchange.json");
        let rows = parse_tickers_json(json).unwrap();
        assert_eq!(rows[0].cik, "0000320193");
        assert_eq!(rows[0].ticker, "AAPL");
    }

    #[test]
    fn primary_listings_prefers_common_over_preferred() {
        let map = primary_listings(&[
            co("883948", "AUB-PA", "NYSE"),
            co("883948", "AUB", "NYSE"),
            co("19617", "JPM-PM", "NYSE"),
            co("19617", "AMJB", "NYSE"),
            co("19617", "JPM", "NYSE"),
            co("798941", "FCNCP", "Nasdaq"),
            co("798941", "FCNCB", "OTC"),
            co("798941", "FCNCO", "Nasdaq"),
            co("798941", "FCNCA", "Nasdaq"),
        ]);
        assert_eq!(map.get("0000883948").map(String::as_str), Some("AUB"));
        assert_eq!(map.get("0000019617").map(String::as_str), Some("JPM"));
        assert_eq!(map.get("0000798941").map(String::as_str), Some("FCNCA"));
    }
}
