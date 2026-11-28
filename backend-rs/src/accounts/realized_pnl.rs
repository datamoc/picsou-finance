//! RealizedPnlService: realized profit and loss from the ordered BUY/SELL stream, supporting
//! per-account cost basis accounting methods (`AVERAGE`, `FIFO`, `LIFO`, `HIFO`) and stock-split
//! adjustments (`stock_split` table). Computed on every read in the account's own currency.

use std::collections::{HashMap, HashSet};

use axum::extract::{Path as AxumPath, State};
use chrono::NaiveDate;
use http::StatusCode;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::auth::Auth;
use crate::db::DbFromRow;
use crate::error::{AppError, AppResult};
use crate::http::json::{Errors, Json, Path, Validate};
use crate::ledger::decimal::{HALF_UP, divide, set_scale};
use crate::ledger::model::{Transaction, TransactionType};
use crate::ledger::sync_values::java_is_blank;
use crate::ledger::{accounts, transactions};
use crate::routes::Routes;

const SCALE: u32 = 8;

pub fn register(routes: &mut Routes) {
    routes
        .get("/api/accounts/{id}/realized-pnl", realized_pnl)
        .patch("/api/accounts/{id}/cost-basis-method", update_cost_basis_method)
        .get("/api/stock-splits", list_stock_splits)
        .post("/api/stock-splits", create_stock_split)
        .delete("/api/stock-splits/{id}", delete_stock_split);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CostBasisMethod {
    Average,
    Fifo,
    Lifo,
    Hifo,
}

impl CostBasisMethod {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "AVERAGE" => Some(Self::Average),
            "FIFO" => Some(Self::Fifo),
            "LIFO" => Some(Self::Lifo),
            "HIFO" => Some(Self::Hifo),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Average => "AVERAGE",
            Self::Fifo => "FIFO",
            Self::Lifo => "LIFO",
            Self::Hifo => "HIFO",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SplitEvent {
    pub ticker: String,
    pub split_date: NaiveDate,
    pub ratio_numerator: Decimal,
    pub ratio_denominator: Decimal,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RealizedPnlResponse {
    pub currency: String,
    pub method: CostBasisMethod,
    #[serde(with = "crate::accounts::jdecimal")]
    pub realized_total: Decimal,
    pub by_ticker: Vec<TickerRealized>,
    pub lots: Vec<ClosedLot>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TickerRealized {
    pub ticker: String,
    pub name: Option<String>,
    #[serde(with = "crate::accounts::jdecimal")]
    pub realized: Decimal,
    #[serde(with = "crate::accounts::jdecimal")]
    pub quantity_sold: Decimal,
    #[serde(with = "crate::accounts::jdecimal")]
    pub proceeds: Decimal,
    #[serde(with = "crate::accounts::jdecimal")]
    pub cost_basis: Decimal,
    pub warning: bool,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClosedLot {
    pub ticker: String,
    pub name: Option<String>,
    pub date: NaiveDate,
    #[serde(with = "crate::accounts::jdecimal")]
    pub quantity: Decimal,
    #[serde(with = "crate::accounts::jdecimal")]
    pub avg_cost: Decimal,
    #[serde(with = "crate::accounts::jdecimal")]
    pub proceeds: Decimal,
    #[serde(with = "crate::accounts::jdecimal")]
    pub realized: Decimal,
}

fn non_negative(v: Decimal) -> Decimal {
    if v < Decimal::ZERO { Decimal::ZERO } else { v }
}

fn scale(v: Decimal) -> Decimal {
    set_scale(v, SCALE, HALF_UP)
}

#[derive(Default)]
struct Realized {
    realized: Decimal,
    quantity_sold: Decimal,
    proceeds: Decimal,
    cost: Decimal,
}

#[derive(Debug, Clone)]
struct OpenLot {
    qty: Decimal,
    unit_cost: Decimal,
}

/// Backward-compatible moving-average entry point used by existing callers and tests.
pub fn compute(currency: String, stream: &[Transaction]) -> RealizedPnlResponse {
    compute_with_method(currency, stream, CostBasisMethod::Average, &[])
}

/// Computes realized P&L using the chosen cost-basis accounting method (`AVERAGE`, `FIFO`, `LIFO`,
/// or `HIFO`) and applying any stock splits in chronological order before subsequent trades.
pub fn compute_with_method(
    currency: String,
    stream: &[Transaction],
    method: CostBasisMethod,
    splits: &[SplitEvent],
) -> RealizedPnlResponse {
    // Group splits by uppercased ticker, ordered chronologically.
    let mut splits_by_ticker: HashMap<String, Vec<&SplitEvent>> = HashMap::new();
    for sp in splits {
        if sp.ratio_numerator > Decimal::ZERO && sp.ratio_denominator > Decimal::ZERO {
            splits_by_ticker
                .entry(sp.ticker.trim().to_uppercase())
                .or_default()
                .push(sp);
        }
    }
    for list in splits_by_ticker.values_mut() {
        list.sort_by_key(|s| s.split_date);
    }

    let mut applied_splits: HashMap<String, usize> = HashMap::new();
    let mut open_lots: HashMap<&str, Vec<OpenLot>> = HashMap::new();
    let mut latest_name: HashMap<&str, String> = HashMap::new();
    let mut order: Vec<&str> = Vec::new();
    let mut by_ticker: HashMap<&str, Realized> = HashMap::new();
    let mut warnings: HashSet<&str> = HashSet::new();
    let mut closed_lots = Vec::new();

    for tx in stream {
        let Some(ticker) = tx.ticker.as_deref().filter(|t| !java_is_blank(t)) else {
            continue;
        };
        let Some(qty) = tx.quantity.filter(|q| *q > Decimal::ZERO) else {
            continue;
        };
        if let Some(name) = &tx.name {
            latest_name.insert(ticker, name.clone());
        }

        // Apply any stock splits whose split_date <= tx.date to currently open lots.
        let tk_upper = ticker.trim().to_uppercase();
        if let Some(ticker_splits) = splits_by_ticker.get(&tk_upper) {
            let cursor = applied_splits.entry(tk_upper).or_insert(0);
            while *cursor < ticker_splits.len() && ticker_splits[*cursor].split_date <= tx.date {
                let sp = ticker_splits[*cursor];
                let factor = divide(sp.ratio_numerator, sp.ratio_denominator, SCALE, HALF_UP);
                if factor > Decimal::ZERO
                    && let Some(lots) = open_lots.get_mut(ticker)
                {
                    for lot in lots.iter_mut() {
                        lot.qty = scale(lot.qty * factor);
                        lot.unit_cost = divide(lot.unit_cost, factor, SCALE, HALF_UP);
                    }
                }
                *cursor += 1;
            }
        }

        let price = tx.price_per_unit.unwrap_or_default();
        let fees = tx.fees.unwrap_or_default();

        if tx.tx_type == Some(TransactionType::Buy) {
            let total_cost = qty * price + fees;
            let unit_cost = divide(total_cost, qty, SCALE, HALF_UP);
            let lots = open_lots.entry(ticker).or_default();
            if method == CostBasisMethod::Average {
                if let Some(single) = lots.first_mut() {
                    let prev_val = single.qty * single.unit_cost;
                    let new_qty = single.qty + qty;
                    single.unit_cost = divide(prev_val + total_cost, new_qty, SCALE, HALF_UP);
                    single.qty = new_qty;
                } else {
                    lots.push(OpenLot { qty, unit_cost });
                }
            } else {
                lots.push(OpenLot { qty, unit_cost });
            }
            continue;
        }

        // SELL: consume open lots according to method.
        let lots = open_lots.entry(ticker).or_default();
        let held_qty: Decimal = lots.iter().map(|l| l.qty).sum();
        let over_sell = qty > held_qty;
        let mut to_consume = if over_sell { non_negative(held_qty) } else { qty };
        let mut cost_out = Decimal::ZERO;

        match method {
            CostBasisMethod::Average => {
                let avg = lots
                    .first()
                    .map(|l| l.unit_cost)
                    .unwrap_or(Decimal::ZERO);
                cost_out = scale(avg * to_consume);
                if let Some(single) = lots.first_mut() {
                    single.qty = non_negative(single.qty - qty);
                    if single.qty.is_zero() {
                        lots.clear();
                    }
                }
            }
            CostBasisMethod::Fifo | CostBasisMethod::Lifo | CostBasisMethod::Hifo => {
                while to_consume > Decimal::ZERO && !lots.is_empty() {
                    let pick_idx = match method {
                        CostBasisMethod::Fifo => 0,
                        CostBasisMethod::Lifo => lots.len() - 1,
                        CostBasisMethod::Hifo => lots
                            .iter()
                            .enumerate()
                            .max_by(|(_, a), (_, b)| a.unit_cost.cmp(&b.unit_cost))
                            .map(|(idx, _)| idx)
                            .unwrap_or(0),
                        CostBasisMethod::Average => unreachable!(),
                    };
                    let take = to_consume.min(lots[pick_idx].qty);
                    cost_out += scale(take * lots[pick_idx].unit_cost);
                    lots[pick_idx].qty -= take;
                    to_consume -= take;
                    if lots[pick_idx].qty <= Decimal::ZERO {
                        lots.remove(pick_idx);
                    }
                }
                cost_out = scale(cost_out);
            }
        }

        let costed_qty = if over_sell { non_negative(held_qty) } else { qty };
        let avg_cost = if costed_qty > Decimal::ZERO {
            divide(cost_out, costed_qty, SCALE, HALF_UP)
        } else {
            Decimal::ZERO
        };
        let proceeds = scale(qty * price - fees);
        let realized = scale(proceeds - cost_out);
        if over_sell {
            warnings.insert(ticker);
        }

        closed_lots.push(ClosedLot {
            ticker: ticker.to_owned(),
            name: latest_name.get(ticker).cloned(),
            date: tx.date,
            quantity: qty,
            avg_cost,
            proceeds,
            realized,
        });
        let entry = by_ticker.entry(ticker).or_insert_with(|| {
            order.push(ticker);
            Realized::default()
        });
        entry.realized += realized;
        entry.quantity_sold += qty;
        entry.proceeds += proceeds;
        entry.cost += cost_out;
    }

    let mut total = Decimal::ZERO;
    let by_ticker: Vec<TickerRealized> = order
        .into_iter()
        .map(|ticker| {
            let r = &by_ticker[ticker];
            total += r.realized;
            TickerRealized {
                ticker: ticker.to_owned(),
                name: latest_name.get(ticker).cloned(),
                realized: r.realized,
                quantity_sold: r.quantity_sold,
                proceeds: r.proceeds,
                cost_basis: r.cost,
                warning: warnings.contains(ticker),
            }
        })
        .collect();
    RealizedPnlResponse {
        currency,
        method,
        realized_total: scale(total),
        by_ticker,
        lots: closed_lots,
    }
}

#[derive(DbFromRow)]
struct StockSplitRow {
    id: i64,
    ticker: String,
    split_date: NaiveDate,
    ratio_numerator: Decimal,
    ratio_denominator: Decimal,
    note: Option<String>,
}

async fn realized_pnl(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> AppResult<axum::Json<RealizedPnlResponse>> {
    let account = accounts::get_or_throw(&state.db, id, auth.member_id).await?;
    let raw_method: Option<String> = crate::db::query_scalar(
        "SELECT cost_basis_method FROM account WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    let method = raw_method
        .as_deref()
        .and_then(CostBasisMethod::parse)
        .unwrap_or(CostBasisMethod::Average);

    let split_rows: Vec<StockSplitRow> = crate::db::query_as(
        "SELECT id, ticker, split_date, ratio_numerator, ratio_denominator, note \
         FROM stock_split WHERE member_id = $1 ORDER BY split_date ASC, id ASC",
    )
    .bind(auth.member_id)
    .fetch_all(&state.db)
    .await?;
    let splits: Vec<SplitEvent> = split_rows
        .into_iter()
        .map(|r| SplitEvent {
            ticker: r.ticker,
            split_date: r.split_date,
            ratio_numerator: r.ratio_numerator,
            ratio_denominator: r.ratio_denominator,
        })
        .collect();

    let stream = transactions::find_by_account_and_tx_types(
        &state.db,
        id,
        &[TransactionType::Buy, TransactionType::Sell],
    )
    .await?;
    Ok(axum::Json(compute_with_method(
        account.currency,
        &stream,
        method,
        &splits,
    )))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateMethodRequest {
    method: CostBasisMethod,
}

impl Validate for UpdateMethodRequest {
    fn validate(&self, _: &mut Errors) {}
}

async fn update_cost_basis_method(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
    Json(req): Json<UpdateMethodRequest>,
) -> AppResult<StatusCode> {
    let _ = accounts::get_or_throw(&state.db, id, auth.member_id).await?;
    crate::db::query(
        "UPDATE account SET cost_basis_method = $1, updated_at = $2 WHERE id = $3 AND member_id = $4",
    )
    .bind(req.method.as_str())
    .bind(state.clock.now())
    .bind(id)
    .bind(auth.member_id)
    .execute(&state.db)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StockSplitDto {
    id: i64,
    ticker: String,
    split_date: NaiveDate,
    ratio_numerator: f64,
    ratio_denominator: f64,
    note: Option<String>,
}

async fn list_stock_splits(
    State(state): State<AppState>,
    auth: Auth,
) -> AppResult<axum::Json<Vec<StockSplitDto>>> {
    let rows: Vec<StockSplitRow> = crate::db::query_as(
        "SELECT id, ticker, split_date, ratio_numerator, ratio_denominator, note \
         FROM stock_split WHERE member_id = $1 ORDER BY split_date DESC, id DESC",
    )
    .bind(auth.member_id)
    .fetch_all(&state.db)
    .await?;
    Ok(axum::Json(
        rows.into_iter()
            .map(|r| StockSplitDto {
                id: r.id,
                ticker: r.ticker,
                split_date: r.split_date,
                ratio_numerator: r.ratio_numerator.to_f64().unwrap_or(1.0),
                ratio_denominator: r.ratio_denominator.to_f64().unwrap_or(1.0),
                note: r.note,
            })
            .collect(),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateStockSplitRequest {
    ticker: String,
    split_date: NaiveDate,
    ratio_numerator: Decimal,
    ratio_denominator: Option<Decimal>,
    note: Option<String>,
}

impl Validate for CreateStockSplitRequest {
    fn validate(&self, errors: &mut Errors) {
        if self.ticker.trim().is_empty() || self.ticker.chars().count() > 60 {
            errors.add("ticker", "Ticker is required");
        }
        if self.ratio_numerator <= Decimal::ZERO {
            errors.add("ratioNumerator", "ratioNumerator must be > 0");
        }
        if let Some(d) = self.ratio_denominator
            && d <= Decimal::ZERO
        {
            errors.add("ratioDenominator", "ratioDenominator must be > 0");
        }
    }
}

async fn create_stock_split(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<CreateStockSplitRequest>,
) -> AppResult<(StatusCode, axum::Json<serde_json::Value>)> {
    let den = req.ratio_denominator.unwrap_or(Decimal::ONE);
    let id: i64 = crate::db::query_scalar(
        "INSERT INTO stock_split \
         (member_id, ticker, split_date, ratio_numerator, ratio_denominator, note, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(auth.member_id)
    .bind(req.ticker.trim().to_uppercase())
    .bind(req.split_date)
    .bind(req.ratio_numerator)
    .bind(den)
    .bind(req.note.as_deref().map(str::trim).filter(|s| !s.is_empty()))
    .bind(state.clock.now())
    .fetch_one(&state.db)
    .await?;
    Ok((StatusCode::CREATED, axum::Json(serde_json::json!({ "id": id }))))
}

async fn delete_stock_split(
    State(state): State<AppState>,
    auth: Auth,
    AxumPath(id): AxumPath<i64>,
) -> AppResult<StatusCode> {
    let affected = crate::db::query(
        "DELETE FROM stock_split WHERE id = $1 AND member_id = $2",
    )
    .bind(id)
    .bind(auth.member_id)
    .execute(&state.db)
    .await?
    .rows_affected();
    if affected == 0 {
        return Err(AppError::NotFound("Stock split not found".to_owned()));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn mk_tx(
        date: (i32, u32, u32),
        tx_type: TransactionType,
        ticker: &str,
        qty: i64,
        price: i64,
    ) -> Transaction {
        Transaction {
            id: 0,
            account_id: 1,
            date: NaiveDate::from_ymd_opt(date.0, date.1, date.2).unwrap(),
            description: String::new(),
            amount: Decimal::from(qty * price),
            kind: None,
            category: None,
            category_id: None,
            category_manual: false,
            counterparty: None,
            merchant_label: None,
            merchant_brand_id: None,
            ai_suggested_category_id: None,
            ai_confidence: None,
            external_id: None,
            recurring_series_id: None,
            native_currency: "EUR".into(),
            created_at: Utc::now(),
            is_manual: true,
            budget_neutral: false,
            external_transaction_id: None,
            tx_type: Some(tx_type),
            ticker: Some(ticker.into()),
            name: Some(ticker.into()),
            quantity: Some(Decimal::from(qty)),
            price_per_unit: Some(Decimal::from(price)),
            fees: Some(Decimal::ZERO),
        }
    }

    #[test]
    fn fifo_lifo_hifo_and_average_produce_expected_realized_gains() {
        // Buy 10 @ 100, Buy 10 @ 150, Buy 10 @ 120 -> Sell 10 @ 200
        let stream = vec![
            mk_tx((2026, 1, 1), TransactionType::Buy, "CW8.PA", 10, 100),
            mk_tx((2026, 2, 1), TransactionType::Buy, "CW8.PA", 10, 150),
            mk_tx((2026, 3, 1), TransactionType::Buy, "CW8.PA", 10, 120),
            mk_tx((2026, 4, 1), TransactionType::Sell, "CW8.PA", 10, 200),
        ];

        let fifo = compute_with_method("EUR".into(), &stream, CostBasisMethod::Fifo, &[]);
        assert_eq!(fifo.realized_total, scale(Decimal::from(1000))); // 2000 - 1000

        let lifo = compute_with_method("EUR".into(), &stream, CostBasisMethod::Lifo, &[]);
        assert_eq!(lifo.realized_total, scale(Decimal::from(800))); // 2000 - 1200

        let hifo = compute_with_method("EUR".into(), &stream, CostBasisMethod::Hifo, &[]);
        assert_eq!(hifo.realized_total, scale(Decimal::from(500))); // 2000 - 1500
    }

    #[test]
    fn stock_split_doubles_quantity_and_halves_unit_cost() {
        // Buy 5 @ 200 (cost 1000) -> 2:1 split -> now 10 @ 100 -> Sell 10 @ 150 => +500 gain
        let stream = vec![
            mk_tx((2026, 1, 1), TransactionType::Buy, "NVDA", 5, 200),
            mk_tx((2026, 6, 1), TransactionType::Sell, "NVDA", 10, 150),
        ];
        let splits = vec![SplitEvent {
            ticker: "NVDA".into(),
            split_date: NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
            ratio_numerator: Decimal::from(2),
            ratio_denominator: Decimal::from(1),
        }];
        let res = compute_with_method("EUR".into(), &stream, CostBasisMethod::Fifo, &splits);
        assert_eq!(res.realized_total, scale(Decimal::from(500)));
        assert!(!res.by_ticker[0].warning);
    }
}
