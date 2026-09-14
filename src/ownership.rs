//! Ownership XML inside a Form 4 complete-submission `.txt`.
//! Capture non-derivative table rows whose transaction code is `P` only.

use anyhow::{bail, Result};

use crate::index::normalize_date;
use crate::tickers::pad_cik;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Purchase {
    pub trade_id: String,
    pub accession: String,
    pub cik: String,
    pub ticker: Option<String>,
    pub company_name: String,
    pub form: String,
    pub is_amendment: i64,
    pub filed_date: String,
    pub owner_cik: String,
    pub rpt_owner_name: String,
    pub officer_title: Option<String>,
    pub is_director: i64,
    pub is_officer: i64,
    pub transaction_date: String,
    pub security_title: String,
    pub transaction_shares: String,
    pub transaction_price: Option<String>,
    pub acquired_disposed: Option<String>,
    pub direct_or_indirect: Option<String>,
    pub filename: String,
    pub source: String,
}

#[allow(clippy::too_many_arguments)]
pub fn parse_purchases(
    body: &str,
    accession: &str,
    issuer_cik: &str,
    company_name: &str,
    form: &str,
    filed_date: &str,
    filename: &str,
    ticker: Option<String>,
) -> Result<Vec<Purchase>> {
    let Some(doc) = ownership_document(body) else {
        bail!("no ownershipDocument in {filename}");
    };
    let owners = parse_owners(doc);
    let table = tag_inner(doc, "nonDerivativeTable").unwrap_or("");
    let is_amendment = i64::from(form.eq_ignore_ascii_case("4/A"));
    let mut out = Vec::new();
    for owner in &owners {
        for tx in each_tagged(table, "nonDerivativeTransaction") {
            let code = tag_text(tx, "transactionCode").unwrap_or_default();
            if !code.eq_ignore_ascii_case("P") {
                continue;
            }
            let Some(transaction_date) =
                tag_text(tx, "transactionDate").and_then(|s| normalize_date(&s))
            else {
                continue;
            };
            let security_title = collapse_ws(&tag_text(tx, "securityTitle").unwrap_or_default());
            if security_title.is_empty() {
                continue;
            }
            let transaction_shares =
                collapse_ws(&tag_text(tx, "transactionShares").unwrap_or_default());
            if transaction_shares.is_empty() {
                continue;
            }
            let transaction_price = nonempty(tag_text(tx, "transactionPricePerShare"));
            let acquired_disposed = nonempty(tag_text(tx, "transactionAcquiredDisposedCode"));
            let direct_or_indirect = nonempty(tag_text(tx, "directOrIndirectOwnership"));
            let owner_cik = owner.cik.clone();
            let trade_id = make_trade_id(
                accession,
                &owner_cik,
                &transaction_date,
                &security_title,
                &transaction_shares,
                transaction_price.as_deref().unwrap_or(""),
                direct_or_indirect.as_deref().unwrap_or(""),
            );
            out.push(Purchase {
                trade_id,
                accession: accession.to_string(),
                cik: pad_cik(issuer_cik),
                ticker: ticker.clone(),
                company_name: company_name.to_string(),
                form: form.to_string(),
                is_amendment,
                filed_date: filed_date.to_string(),
                owner_cik,
                rpt_owner_name: owner.name.clone(),
                officer_title: owner.title.clone(),
                is_director: owner.is_director,
                is_officer: owner.is_officer,
                transaction_date,
                security_title,
                transaction_shares,
                transaction_price,
                acquired_disposed,
                direct_or_indirect,
                filename: filename.to_string(),
                source: "txt".into(),
            });
        }
    }
    Ok(out)
}

pub fn make_trade_id(
    accession: &str,
    owner_cik: &str,
    transaction_date: &str,
    security_title: &str,
    shares: &str,
    price: &str,
    direct_or_indirect: &str,
) -> String {
    format!(
        "{accession}|{owner_cik}|{transaction_date}|{security_title}|{shares}|{price}|{direct_or_indirect}"
    )
}

#[derive(Debug, Clone, Default)]
struct Owner {
    cik: String,
    name: String,
    title: Option<String>,
    is_director: i64,
    is_officer: i64,
}

fn parse_owners(doc: &str) -> Vec<Owner> {
    let blocks = each_tagged(doc, "reportingOwner");
    if blocks.is_empty() {
        return vec![Owner::default()];
    }
    blocks
        .into_iter()
        .map(|block| Owner {
            cik: pad_cik(&tag_text(block, "rptOwnerCik").unwrap_or_default()),
            name: collapse_ws(&tag_text(block, "rptOwnerName").unwrap_or_default()),
            title: nonempty(tag_text(block, "officerTitle")),
            is_director: flag(tag_text(block, "isDirector")),
            is_officer: flag(tag_text(block, "isOfficer")),
        })
        .collect()
}

fn ownership_document(body: &str) -> Option<&str> {
    tag_inner(body, "ownershipDocument")
}

fn tag_inner<'a>(hay: &'a str, tag: &str) -> Option<&'a str> {
    let (start, close_len) = find_open_close(hay, tag)?;
    Some(&hay[start.0..start.1.saturating_sub(close_len)])
}

/// Returns (inner_start..close_end, close_tag_len) where the slice
/// `hay[inner_start..close_end-close_tag_len]` is the inner XML.
fn find_open_close(hay: &str, tag: &str) -> Option<((usize, usize), usize)> {
    let bytes = hay.as_bytes();
    let open = format!("<{tag}").to_ascii_lowercase();
    let close = format!("</{tag}>").to_ascii_lowercase();
    let i = find_ci(bytes, open.as_bytes())?;
    let gt_rel = hay[i..].find('>')?;
    let inner_start = i + gt_rel + 1;
    let end_rel = find_ci(&bytes[inner_start..], close.as_bytes())?;
    let close_end = inner_start + end_rel + close.len();
    Some(((inner_start, close_end), close.len()))
}

fn each_tagged<'a>(hay: &'a str, tag: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut offset = 0;
    let open = format!("<{tag}").to_ascii_lowercase();
    let close = format!("</{tag}>").to_ascii_lowercase();
    let bytes = hay.as_bytes();
    while let Some(i) = find_ci(&bytes[offset..], open.as_bytes()) {
        let abs = offset + i;
        let Some(gt_rel) = hay[abs..].find('>') else {
            break;
        };
        let inner_start = abs + gt_rel + 1;
        let Some(end_rel) = find_ci(&bytes[inner_start..], close.as_bytes()) else {
            break;
        };
        out.push(&hay[inner_start..inner_start + end_rel]);
        offset = inner_start + end_rel + close.len();
    }
    out
}

fn tag_text(hay: &str, tag: &str) -> Option<String> {
    let inner = tag_inner(hay, tag)?;
    if let Some(v) = tag_inner(inner, "value") {
        let t = collapse_ws(&text_content(v));
        if t.is_empty() {
            return None;
        }
        return Some(t);
    }
    let t = collapse_ws(&text_content(inner));
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

fn text_content(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn nonempty(s: Option<String>) -> Option<String> {
    s.filter(|v| !v.is_empty())
}

fn flag(s: Option<String>) -> i64 {
    match s.as_deref().map(|v| v.trim().to_ascii_lowercase()) {
        Some(v) if matches!(v.as_str(), "1" | "true" | "yes" | "y") => 1,
        _ => 0,
    }
}

fn find_ci(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| {
        w.iter()
            .zip(needle)
            .all(|(a, b)| a.to_ascii_lowercase() == *b)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_code_p_skips_sale_and_derivative() {
        let body = include_str!("../fixtures/aapl-form4-purchase.txt");
        let rows = parse_purchases(
            body,
            "0000320193-26-000200",
            "0000320193",
            "Apple Inc.",
            "4",
            "2026-09-11",
            "edgar/data/320193/0000320193-26-000200.txt",
            Some("AAPL".into()),
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].security_title, "Common Stock");
        assert_eq!(rows[0].transaction_shares, "10000");
        assert_eq!(rows[0].transaction_price.as_deref(), Some("150.25"));
        assert_eq!(rows[0].rpt_owner_name, "Cook Timothy D");
        assert_eq!(rows[0].is_officer, 1);
        assert_eq!(rows[0].is_director, 1);
        assert_eq!(rows[0].direct_or_indirect.as_deref(), Some("D"));
        assert!(rows[0].trade_id.contains("0000320193-26-000200"));
    }

    #[test]
    fn sale_only_filing_yields_no_purchases() {
        let body = include_str!("../fixtures/wmt-form4-sale.txt");
        let rows = parse_purchases(
            body,
            "0000104169-26-000060",
            "0000104169",
            "Walmart Inc.",
            "4",
            "2026-09-11",
            "edgar/data/104169/0000104169-26-000060.txt",
            Some("WMT".into()),
        )
        .unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn missing_ownership_document_is_error() {
        let body = include_str!("../fixtures/aapl-form4-no-xml.txt");
        let err = parse_purchases(
            body,
            "0000320193-26-000299",
            "0000320193",
            "Apple Inc.",
            "4",
            "2026-09-11",
            "edgar/data/320193/0000320193-26-000299.txt",
            Some("AAPL".into()),
        )
        .unwrap_err();
        assert!(err.to_string().contains("no ownershipDocument"));
    }

    #[test]
    fn joint_filing_emits_one_row_per_owner() {
        let body = include_str!("../fixtures/aapl-form4-joint.txt");
        let rows = parse_purchases(
            body,
            "0000320193-26-000210",
            "0000320193",
            "Apple Inc.",
            "4",
            "2026-09-11",
            "edgar/data/320193/0000320193-26-000210.txt",
            Some("AAPL".into()),
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        let names: Vec<&str> = rows.iter().map(|r| r.rpt_owner_name.as_str()).collect();
        assert!(names.contains(&"Cook Timothy D"));
        assert!(names.contains(&"Maestri Luca"));
        assert_ne!(rows[0].owner_cik, rows[1].owner_cik);
        assert_ne!(rows[0].trade_id, rows[1].trade_id);
        assert!(rows.iter().all(|r| r.transaction_shares == "10000"));
    }
}
