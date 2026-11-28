//! Budget transaction splits: divide a single transaction across multiple categories while
//! enforcing exact sum conservation against the parent transaction amount.

use axum::extract::{Path as AxumPath, State};
use http::StatusCode;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::auth::Auth;
use crate::db::DbFromRow;
use crate::error::{AppError, AppResult};
use crate::http::json::{Errors, Json, Validate};
use crate::routes::Routes;

pub fn register(routes: &mut Routes) {
    routes
        .get("/api/budget/transactions/{id}/splits", get_splits)
        .put("/api/budget/transactions/{id}/splits", replace_splits)
        .delete("/api/budget/transactions/{id}/splits", clear_splits);
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitLineView {
    pub id: i64,
    pub category_id: i64,
    pub category_name: String,
    pub amount: f64,
    pub note: Option<String>,
}

#[derive(DbFromRow)]
struct SplitRow {
    id: i64,
    category_id: i64,
    category_name: String,
    amount: Decimal,
    note: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SplitInput {
    category_id: i64,
    amount: Decimal,
    note: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReplaceSplitsRequest {
    splits: Vec<SplitInput>,
}

impl Validate for ReplaceSplitsRequest {
    fn validate(&self, errors: &mut Errors) {
        if self.splits.len() < 2 {
            errors.add("splits", "At least two split lines are required");
        }
        if self.splits.iter().any(|s| s.amount == Decimal::ZERO) {
            errors.add("splits", "Each split line must have a non-zero amount");
        }
    }
}

pub fn validate_split_sum(parent_amount: Decimal, parts: &[Decimal]) -> Result<(), String> {
    let sum: Decimal = parts.iter().copied().sum();
    let diff = (parent_amount - sum).abs();
    if diff > Decimal::new(1, 2) {
        return Err(format!(
            "Split sum ({sum}) does not match transaction amount ({parent_amount})"
        ));
    }
    Ok(())
}

async fn assert_owned_tx(state: &AppState, tx_id: i64, member_id: i64) -> AppResult<Decimal> {
    crate::db::query_scalar::<Decimal>(
        "SELECT t.amount FROM transaction t \
         JOIN account a ON a.id = t.account_id \
         WHERE t.id = $1 AND a.member_id = $2 AND a.deleted_at IS NULL",
    )
    .bind(tx_id)
    .bind(member_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Transaction not found".to_owned()))
}

async fn get_splits(
    State(state): State<AppState>,
    auth: Auth,
    AxumPath(tx_id): AxumPath<i64>,
) -> AppResult<axum::Json<Vec<SplitLineView>>> {
    let _ = assert_owned_tx(&state, tx_id, auth.member_id).await?;
    let rows: Vec<SplitRow> = crate::db::query_as(
        "SELECT s.id, s.category_id, c.name AS category_name, s.amount, s.note \
         FROM transaction_split s \
         JOIN category c ON c.id = s.category_id \
         WHERE s.transaction_id = $1 ORDER BY s.id",
    )
    .bind(tx_id)
    .fetch_all(&state.db)
    .await?;

    Ok(axum::Json(
        rows.into_iter()
            .map(|r| SplitLineView {
                id: r.id,
                category_id: r.category_id,
                category_name: r.category_name,
                amount: r.amount.to_f64().unwrap_or(0.0),
                note: r.note,
            })
            .collect(),
    ))
}

async fn replace_splits(
    State(state): State<AppState>,
    auth: Auth,
    AxumPath(tx_id): AxumPath<i64>,
    Json(req): Json<ReplaceSplitsRequest>,
) -> AppResult<StatusCode> {
    let parent_amount = assert_owned_tx(&state, tx_id, auth.member_id).await?;
    let amounts: Vec<Decimal> = req.splits.iter().map(|s| s.amount).collect();
    validate_split_sum(parent_amount, &amounts).map_err(AppError::BadRequest)?;

    let cat_ids: Vec<i64> = req.splits.iter().map(|s| s.category_id).collect();
    let owned_cats: Vec<i64> = crate::db::query_scalar(
        "SELECT id FROM category WHERE member_id = $1 AND id = ANY($2)",
    )
    .bind(auth.member_id)
    .bind(&cat_ids)
    .fetch_all(&state.db)
    .await?;
    for cid in &cat_ids {
        if !owned_cats.contains(cid) {
            return Err(AppError::BadRequest(format!("Unknown category {cid}")));
        }
    }

    let now = state.clock.now();
    crate::db::query("DELETE FROM transaction_split WHERE transaction_id = $1")
        .bind(tx_id)
        .execute(&state.db)
        .await?;

    for line in &req.splits {
        crate::db::query(
            "INSERT INTO transaction_split (transaction_id, category_id, amount, note, created_at) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(tx_id)
        .bind(line.category_id)
        .bind(line.amount)
        .bind(line.note.as_deref().map(str::trim).filter(|s| !s.is_empty()))
        .bind(now)
        .execute(&state.db)
        .await?;
    }

    Ok(StatusCode::NO_CONTENT)
}

async fn clear_splits(
    State(state): State<AppState>,
    auth: Auth,
    AxumPath(tx_id): AxumPath<i64>,
) -> AppResult<StatusCode> {
    let _ = assert_owned_tx(&state, tx_id, auth.member_id).await?;
    crate::db::query("DELETE FROM transaction_split WHERE transaction_id = $1")
        .bind(tx_id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn enforces_exact_split_sum() {
        let total = Decimal::from_str("-100.00").unwrap();
        let ok = [
            Decimal::from_str("-65.50").unwrap(),
            Decimal::from_str("-34.50").unwrap(),
        ];
        assert!(validate_split_sum(total, &ok).is_ok());

        let bad = [
            Decimal::from_str("-60.00").unwrap(),
            Decimal::from_str("-30.00").unwrap(),
        ];
        assert!(validate_split_sum(total, &bad).is_err());
    }
}
