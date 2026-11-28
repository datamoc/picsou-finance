//! Consolidated Capital Income view: aggregates historical dividends, interest, coupons, and
//! SCPI distributions by month, currency, and asset with year-over-year comparison and explicit
//! alerts for unconverted foreign currencies.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use axum::extract::{Query, State};
use chrono::{Datelike, NaiveDate};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::auth::Auth;
use crate::db::DbFromRow;
use crate::error::AppResult;
use crate::routes::Routes;

pub fn register(routes: &mut Routes) {
    routes.get("/api/wealth/income", get_income_summary);
}

#[derive(Debug, Deserialize)]
struct IncomeQuery {
    year: Option<i32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IncomeMonthlyBucket {
    pub month: String,
    pub dividends_eur: f64,
    pub interest_eur: f64,
    pub scpi_eur: f64,
    pub total_eur: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IncomeByCurrency {
    pub currency: String,
    pub native_amount: f64,
    pub amount_eur: f64,
    pub converted: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IncomeByAsset {
    pub label: String,
    pub ticker: Option<String>,
    pub kind: &'static str,
    pub total_eur: f64,
    pub payment_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IncomeSummaryResponse {
    pub year: i32,
    pub total_eur: f64,
    pub previous_year_total_eur: f64,
    pub yoy_change_pct: Option<f64>,
    pub monthly_average_eur: f64,
    pub monthly: Vec<IncomeMonthlyBucket>,
    pub by_currency: Vec<IncomeByCurrency>,
    pub by_asset: Vec<IncomeByAsset>,
    pub unconverted_currencies: Vec<String>,
}

#[derive(DbFromRow)]
struct IncomeTxRow {
    date: NaiveDate,
    amount: Decimal,
    currency: String,
    tx_type: Option<String>,
    ticker: Option<String>,
    name: Option<String>,
    description: Option<String>,
    account_type: String,
    account_name: String,
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn classify_income_kind(row: &IncomeTxRow) -> Option<&'static str> {
    if row.amount <= Decimal::ZERO {
        return None;
    }
    if row.account_type == "SCPI" {
        return Some("SCPI");
    }
    if let Some(tt) = row.tx_type.as_deref() {
        if tt.eq_ignore_ascii_case("DIVIDEND") {
            return Some("DIVIDEND");
        }
        if tt.eq_ignore_ascii_case("INTEREST") {
            return Some("INTEREST");
        }
    }
    let hay = row
        .description
        .as_deref()
        .unwrap_or("")
        .to_lowercase();
    if hay.contains("dividende") || hay.contains("dividend") || hay.contains("coupon") {
        return Some("DIVIDEND");
    }
    if hay.contains("intérêts") || hay.contains("interets") || hay.contains("interest") {
        return Some("INTEREST");
    }
    None
}

async fn get_income_summary(
    State(state): State<AppState>,
    auth: Auth,
    Query(q): Query<IncomeQuery>,
) -> AppResult<axum::Json<IncomeSummaryResponse>> {
    let year = q.year.unwrap_or_else(|| state.clock.today().year());
    let prev_start = NaiveDate::from_ymd_opt(year - 1, 1, 1).unwrap_or(state.clock.today());
    let year_end = NaiveDate::from_ymd_opt(year, 12, 31).unwrap_or(state.clock.today());

    let rows: Vec<IncomeTxRow> = crate::db::query_as(
        "SELECT t.date, t.amount, t.native_currency AS currency, t.tx_type, t.ticker, t.name, t.description, \
                a.type::text AS account_type, a.name AS account_name \
         FROM transaction t \
         JOIN account a ON a.id = t.account_id \
         WHERE a.member_id = $1 AND a.deleted_at IS NULL AND a.hidden = false \
           AND t.date >= $2 AND t.date <= $3 AND t.amount > 0 \
         ORDER BY t.date ASC",
    )
    .bind(auth.member_id)
    .bind(prev_start)
    .bind(year_end)
    .fetch_all(&state.db)
    .await?;

    let mut months: BTreeMap<u32, (f64, f64, f64)> = BTreeMap::new();
    for m in 1..=12u32 {
        months.insert(m, (0.0, 0.0, 0.0));
    }

    let mut cur_map: HashMap<String, (f64, f64, bool)> = HashMap::new();
    let mut asset_map: HashMap<(String, Option<String>, &'static str), (f64, usize)> = HashMap::new();
    let mut unconverted = BTreeSet::new();
    let mut prev_year_total = 0.0_f64;

    for r in &rows {
        let Some(kind) = classify_income_kind(r) else {
            continue;
        };
        let native = r.amount.to_f64().unwrap_or(0.0);
        let ccy = r.currency.trim().to_uppercase();
        let converted = ccy == "EUR";
        if !converted && r.date.year() == year {
            unconverted.insert(ccy.clone());
        }
        let eur = native;

        if r.date.year() == year - 1 {
            prev_year_total += eur;
            continue;
        }
        if r.date.year() != year {
            continue;
        }

        let bucket = months.entry(r.date.month()).or_insert((0.0, 0.0, 0.0));
        match kind {
            "DIVIDEND" => bucket.0 += eur,
            "INTEREST" => bucket.1 += eur,
            "SCPI" => bucket.2 += eur,
            _ => {}
        }

        let c_entry = cur_map.entry(ccy).or_insert((0.0, 0.0, converted));
        c_entry.0 += native;
        c_entry.1 += eur;

        let label = r
            .name
            .clone()
            .or_else(|| r.ticker.clone())
            .unwrap_or_else(|| r.account_name.clone());
        let a_entry = asset_map
            .entry((label, r.ticker.clone(), kind))
            .or_insert((0.0, 0));
        a_entry.0 += eur;
        a_entry.1 += 1;
    }

    let monthly: Vec<IncomeMonthlyBucket> = months
        .into_iter()
        .map(|(m, (div, int, scpi))| IncomeMonthlyBucket {
            month: format!("{year}-{m:02}"),
            dividends_eur: round2(div),
            interest_eur: round2(int),
            scpi_eur: round2(scpi),
            total_eur: round2(div + int + scpi),
        })
        .collect();

    let total_eur: f64 = monthly.iter().map(|b| b.total_eur).sum();
    let yoy_change_pct = if prev_year_total > 0.0 {
        Some((((total_eur - prev_year_total) / prev_year_total) * 1000.0).round() / 10.0)
    } else {
        None
    };

    let mut by_currency: Vec<IncomeByCurrency> = cur_map
        .into_iter()
        .map(|(currency, (native_amount, amount_eur, converted))| IncomeByCurrency {
            currency,
            native_amount: round2(native_amount),
            amount_eur: round2(amount_eur),
            converted,
        })
        .collect();
    by_currency.sort_by(|a, b| b.amount_eur.total_cmp(&a.amount_eur));

    let mut by_asset: Vec<IncomeByAsset> = asset_map
        .into_iter()
        .map(|((label, ticker, kind), (total, count))| IncomeByAsset {
            label,
            ticker,
            kind,
            total_eur: round2(total),
            payment_count: count,
        })
        .collect();
    by_asset.sort_by(|a, b| b.total_eur.total_cmp(&a.total_eur));

    Ok(axum::Json(IncomeSummaryResponse {
        year,
        total_eur: round2(total_eur),
        previous_year_total_eur: round2(prev_year_total),
        yoy_change_pct,
        monthly_average_eur: round2(total_eur / 12.0),
        monthly,
        by_currency,
        by_asset,
        unconverted_currencies: unconverted.into_iter().collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_dividends_interest_and_scpi_correctly() {
        let mk = |tx_type: Option<&str>, desc: Option<&str>, acc_type: &str| IncomeTxRow {
            date: NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(),
            amount: Decimal::from(50),
            currency: "EUR".into(),
            tx_type: tx_type.map(str::to_owned),
            ticker: None,
            name: None,
            description: desc.map(str::to_owned),
            account_type: acc_type.into(),
            account_name: "Test".into(),
        };
        assert_eq!(classify_income_kind(&mk(Some("DIVIDEND"), None, "PEA")), Some("DIVIDEND"));
        assert_eq!(classify_income_kind(&mk(Some("INTEREST"), None, "LIVRET_A")), Some("INTEREST"));
        assert_eq!(classify_income_kind(&mk(None, Some("Loyers T1"), "SCPI")), Some("SCPI"));
        assert_eq!(classify_income_kind(&mk(None, Some("Dividende Total"), "COMPTE_TITRES")), Some("DIVIDEND"));
        assert_eq!(classify_income_kind(&mk(None, Some("Virement"), "CHECKING")), None);
    }
}
