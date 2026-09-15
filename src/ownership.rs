//! Ownership XML inside a Form 4 complete-submission `.txt`.
//! Capture non-derivative table rows whose transaction code is `P` only.

use anyhow::{bail, Context, Result};
use roxmltree::Node;

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
    ticker_fallback: Option<String>,
) -> Result<Vec<Purchase>> {
    let Some(xml) = extract_ownership_xml(body) else {
        bail!("no ownershipDocument in {filename}");
    };
    let doc = roxmltree::Document::parse(xml)
        .with_context(|| format!("parse ownership XML in {filename}"))?;
    let root = doc.root_element();
    if !local_eq(root, "ownershipDocument") {
        bail!("no ownershipDocument in {filename}");
    }

    let owners = parse_owners(root, filename)?;
    let ticker = issuer_trading_symbol(root).or(ticker_fallback);
    let is_amendment = i64::from(form.eq_ignore_ascii_case("4/A"));
    let txs = non_derivative_transactions(root);
    let mut out = Vec::new();
    for owner in &owners {
        for (tx_index, tx) in txs.iter().enumerate() {
            let code = tag_text(*tx, "transactionCode").unwrap_or_default();
            if !code.eq_ignore_ascii_case("P") {
                continue;
            }
            let Some(transaction_date) =
                tag_text(*tx, "transactionDate").and_then(|s| normalize_date(&s))
            else {
                tracing::warn!(
                    filename,
                    accession,
                    tx_index,
                    "skipping code-P row: missing transactionDate"
                );
                continue;
            };
            let security_title = collapse_ws(&tag_text(*tx, "securityTitle").unwrap_or_default());
            if security_title.is_empty() {
                tracing::warn!(
                    filename,
                    accession,
                    tx_index,
                    "skipping code-P row: missing securityTitle"
                );
                continue;
            }
            let transaction_shares =
                collapse_ws(&tag_text(*tx, "transactionShares").unwrap_or_default());
            if transaction_shares.is_empty() {
                tracing::warn!(
                    filename,
                    accession,
                    tx_index,
                    "skipping code-P row: missing transactionShares"
                );
                continue;
            }
            let transaction_price = nonempty(tag_text(*tx, "transactionPricePerShare"));
            let acquired_disposed = nonempty(tag_text(*tx, "transactionAcquiredDisposedCode"));
            let direct_or_indirect = nonempty(tag_text(*tx, "directOrIndirectOwnership"));
            let owner_cik = owner.cik.clone();
            let trade_id = make_trade_id(
                accession,
                &owner_cik,
                tx_index,
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

#[allow(clippy::too_many_arguments)]
pub fn make_trade_id(
    accession: &str,
    owner_cik: &str,
    tx_index: usize,
    transaction_date: &str,
    security_title: &str,
    shares: &str,
    price: &str,
    direct_or_indirect: &str,
) -> String {
    format!(
        "{accession}|{owner_cik}|{tx_index}|{transaction_date}|{security_title}|{shares}|{price}|{direct_or_indirect}"
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

fn parse_owners<'a, 'input>(root: Node<'a, 'input>, filename: &str) -> Result<Vec<Owner>> {
    let owners: Vec<Owner> = root
        .descendants()
        .filter(|n| local_eq(*n, "reportingOwner"))
        .map(|block| Owner {
            cik: pad_cik(&tag_text(block, "rptOwnerCik").unwrap_or_default()),
            name: collapse_ws(&tag_text(block, "rptOwnerName").unwrap_or_default()),
            title: nonempty(tag_text(block, "officerTitle")),
            is_director: flag(tag_text(block, "isDirector")),
            is_officer: flag(tag_text(block, "isOfficer")),
        })
        .collect();
    if owners.is_empty() {
        bail!("no reportingOwner in {filename}");
    }
    Ok(owners)
}

fn issuer_trading_symbol(root: Node<'_, '_>) -> Option<String> {
    nonempty(tag_text(root, "issuerTradingSymbol")).map(|s| s.to_ascii_uppercase())
}

fn non_derivative_transactions<'a, 'input>(root: Node<'a, 'input>) -> Vec<Node<'a, 'input>> {
    let Some(table) = root
        .descendants()
        .find(|n| local_eq(*n, "nonDerivativeTable"))
    else {
        return Vec::new();
    };
    table
        .children()
        .filter(|n| local_eq(*n, "nonDerivativeTransaction"))
        .collect()
}

fn extract_ownership_xml(body: &str) -> Option<&str> {
    let bytes = body.as_bytes();
    let start = find_ci(bytes, b"<ownershipdocument")?;
    let close = b"</ownershipdocument>";
    let rel = find_ci(&bytes[start..], close)?;
    Some(&body[start..start + rel + close.len()])
}

fn local_eq(node: Node<'_, '_>, tag: &str) -> bool {
    node.is_element() && node.tag_name().name().eq_ignore_ascii_case(tag)
}

fn tag_text(scope: Node<'_, '_>, tag: &str) -> Option<String> {
    let el = scope.descendants().find(|n| local_eq(*n, tag))?;
    if let Some(v) = el.children().find(|n| local_eq(*n, "value")) {
        return nonempty(Some(element_text(v)));
    }
    nonempty(Some(element_text(el)))
}

fn element_text(el: Node<'_, '_>) -> String {
    el.descendants()
        .filter(|n| n.is_text())
        .filter_map(|n| n.text())
        .collect::<Vec<_>>()
        .join("")
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn nonempty(s: Option<String>) -> Option<String> {
    s.map(|v| collapse_ws(&v)).filter(|v| !v.is_empty())
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

    fn parse_fixture(
        body: &str,
        accession: &str,
        cik: &str,
        name: &str,
        form: &str,
    ) -> Vec<Purchase> {
        parse_purchases(
            body,
            accession,
            cik,
            name,
            form,
            "2026-09-11",
            "edgar/data/320193/fixture.txt",
            None,
        )
        .unwrap()
    }

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
            None,
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
        assert_eq!(rows[0].ticker.as_deref(), Some("AAPL"));
        assert_eq!(
            rows[0].trade_id,
            "0000320193-26-000200|0001214156|0|2026-09-10|Common Stock|10000|150.25|D"
        );
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
            None,
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
            None,
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        let names: Vec<&str> = rows.iter().map(|r| r.rpt_owner_name.as_str()).collect();
        assert!(names.contains(&"Cook Timothy D"));
        assert!(names.contains(&"Maestri Luca"));
        assert_ne!(rows[0].owner_cik, rows[1].owner_cik);
        assert_ne!(rows[0].trade_id, rows[1].trade_id);
        assert!(rows.iter().all(|r| r.transaction_shares == "10000"));
        assert!(rows.iter().all(|r| r.trade_id.contains("|0|")));
    }

    #[test]
    fn duplicate_same_day_p_rows_get_distinct_trade_ids() {
        let body = include_str!("../fixtures/aapl-form4-two-purchases.txt");
        let rows = parse_purchases(
            body,
            "0000320193-26-000220",
            "0000320193",
            "Apple Inc.",
            "4",
            "2026-09-11",
            "edgar/data/320193/0000320193-26-000220.txt",
            None,
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].transaction_shares, rows[1].transaction_shares);
        assert_eq!(rows[0].transaction_price, rows[1].transaction_price);
        assert_ne!(rows[0].trade_id, rows[1].trade_id);
        assert!(rows[0].trade_id.contains("|0|"));
        assert!(rows[1].trade_id.contains("|1|"));
    }

    #[test]
    fn prefers_issuer_trading_symbol_over_fallback() {
        let body = include_str!("../fixtures/aapl-form4-purchase.txt");
        let rows = parse_purchases(
            body,
            "0000320193-26-000200",
            "0000320193",
            "Apple Inc.",
            "4",
            "2026-09-11",
            "edgar/data/320193/0000320193-26-000200.txt",
            Some("NOTAAPL".into()),
        )
        .unwrap();
        assert_eq!(rows[0].ticker.as_deref(), Some("AAPL"));
    }

    #[test]
    fn uses_ticker_fallback_when_symbol_missing() {
        let body = r#"<ownershipDocument>
  <reportingOwner><rptOwnerCik>0001214156</rptOwnerCik><rptOwnerName>Cook</rptOwnerName></reportingOwner>
  <nonDerivativeTable>
    <nonDerivativeTransaction>
      <securityTitle><value>Common Stock</value></securityTitle>
      <transactionDate><value>2026-09-10</value></transactionDate>
      <transactionCoding><transactionCode>P</transactionCode></transactionCoding>
      <transactionShares><value>1</value></transactionShares>
    </nonDerivativeTransaction>
  </nonDerivativeTable>
</ownershipDocument>"#;
        let rows = parse_purchases(
            body,
            "0000320193-26-000001",
            "0000320193",
            "Apple Inc.",
            "4",
            "2026-09-11",
            "x.txt",
            Some("AAPL".into()),
        )
        .unwrap();
        assert_eq!(rows[0].ticker.as_deref(), Some("AAPL"));
    }

    #[test]
    fn missing_reporting_owner_is_error() {
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
        let err = parse_purchases(
            body,
            "0000320193-26-000001",
            "0000320193",
            "Apple Inc.",
            "4",
            "2026-09-11",
            "x.txt",
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("no reportingOwner"));
    }

    #[test]
    fn incomplete_code_p_row_is_skipped_not_stored() {
        let body = r#"<ownershipDocument>
  <reportingOwner><rptOwnerCik>0001214156</rptOwnerCik><rptOwnerName>Cook</rptOwnerName></reportingOwner>
  <nonDerivativeTable>
    <nonDerivativeTransaction>
      <securityTitle><value>Common Stock</value></securityTitle>
      <transactionCoding><transactionCode>P</transactionCode></transactionCoding>
      <transactionShares><value>1</value></transactionShares>
    </nonDerivativeTransaction>
  </nonDerivativeTable>
</ownershipDocument>"#;
        let rows = parse_fixture(
            body,
            "0000320193-26-000001",
            "0000320193",
            "Apple Inc.",
            "4",
        );
        assert!(rows.is_empty());
    }

    #[test]
    fn decodes_xml_entities() {
        let body = r#"<ownershipDocument>
  <issuerTradingSymbol>AAPL</issuerTradingSymbol>
  <reportingOwner>
    <rptOwnerCik>0001214156</rptOwnerCik>
    <rptOwnerName>Cook &amp; Family</rptOwnerName>
  </reportingOwner>
  <nonDerivativeTable>
    <nonDerivativeTransaction>
      <securityTitle><value>Common Stock</value></securityTitle>
      <transactionDate><value>2026-09-10</value></transactionDate>
      <transactionCoding><transactionCode>P</transactionCode></transactionCoding>
      <transactionShares><value>1</value></transactionShares>
    </nonDerivativeTransaction>
  </nonDerivativeTable>
</ownershipDocument>"#;
        let rows = parse_fixture(
            body,
            "0000320193-26-000001",
            "0000320193",
            "Apple Inc.",
            "4",
        );
        assert_eq!(rows[0].rpt_owner_name, "Cook & Family");
    }
}
