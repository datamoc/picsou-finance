//! Budget spending events: group transactions across categories into named one-off projects
//! (holidays, renovations, moving) with an optional dedicated target budget.

use std::collections::HashMap;

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
use crate::http::json::{Errors, Json, Validate};
use crate::routes::Routes;

pub fn register(routes: &mut Routes) {
    routes
        .get("/api/budget/events", list_events)
        .post("/api/budget/events", create_event)
        .put("/api/budget/events/{id}", update_event)
        .delete("/api/budget/events/{id}", delete_event)
        .post("/api/budget/events/{id}/transactions/{tx_id}", link_transaction)
        .delete("/api/budget/events/{id}/transactions/{tx_id}", unlink_transaction);
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpendingEventView {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub start_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    pub budget_amount: Option<f64>,
    pub color: String,
    pub spent_amount: f64,
    pub transaction_count: usize,
    pub usage_percent: Option<f64>,
}

#[derive(DbFromRow)]
struct EventRow {
    id: i64,
    name: String,
    description: Option<String>,
    start_date: Option<NaiveDate>,
    end_date: Option<NaiveDate>,
    budget_amount: Option<Decimal>,
    color: String,
}

#[derive(DbFromRow)]
struct EventTxRow {
    event_id: i64,
    amount: Decimal,
}

async fn list_events(
    State(state): State<AppState>,
    auth: Auth,
) -> AppResult<axum::Json<Vec<SpendingEventView>>> {
    let rows: Vec<EventRow> = crate::db::query_as(
        "SELECT id, name, description, start_date, end_date, budget_amount, color \
         FROM spending_event WHERE member_id = $1 ORDER BY COALESCE(start_date, '9999-12-31') DESC, id DESC",
    )
    .bind(auth.member_id)
    .fetch_all(&state.db)
    .await?;

    let tx_rows: Vec<EventTxRow> = crate::db::query_as(
        "SELECT setx.event_id, t.amount \
         FROM spending_event_transaction setx \
         JOIN spending_event ev ON ev.id = setx.event_id \
         JOIN transaction t ON t.id = setx.transaction_id \
         WHERE ev.member_id = $1",
    )
    .bind(auth.member_id)
    .fetch_all(&state.db)
    .await?;

    let mut aggs: HashMap<i64, (usize, Decimal)> = HashMap::new();
    for r in tx_rows {
        let entry = aggs.entry(r.event_id).or_insert((0, Decimal::ZERO));
        entry.0 += 1;
        if r.amount < Decimal::ZERO {
            entry.1 += r.amount.abs();
        }
    }

    let out = rows
        .into_iter()
        .map(|ev| {
            let (tx_count, spent_dec) = aggs
                .get(&ev.id)
                .copied()
                .unwrap_or((0, Decimal::ZERO));
            let spent = spent_dec.to_f64().unwrap_or(0.0);
            let budget = ev.budget_amount.and_then(|d| d.to_f64());
            let usage = budget
                .filter(|b| *b > 0.0)
                .map(|b| ((spent / b) * 1000.0).round() / 10.0);
            SpendingEventView {
                id: ev.id,
                name: ev.name,
                description: ev.description,
                start_date: ev.start_date,
                end_date: ev.end_date,
                budget_amount: budget,
                color: ev.color,
                spent_amount: (spent * 100.0).round() / 100.0,
                transaction_count: tx_count,
                usage_percent: usage,
            }
        })
        .collect();

    Ok(axum::Json(out))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpsertEventRequest {
    name: String,
    description: Option<String>,
    start_date: Option<NaiveDate>,
    end_date: Option<NaiveDate>,
    budget_amount: Option<Decimal>,
    color: Option<String>,
}

impl Validate for UpsertEventRequest {
    fn validate(&self, errors: &mut Errors) {
        if self.name.trim().is_empty() || self.name.chars().count() > 120 {
            errors.add("name", "Name must be between 1 and 120 characters");
        }
        if let Some(b) = self.budget_amount
            && b <= Decimal::ZERO
        {
            errors.add("budgetAmount", "budgetAmount must be > 0");
        }
    }
}

async fn create_event(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<UpsertEventRequest>,
) -> AppResult<(StatusCode, axum::Json<serde_json::Value>)> {
    let now = state.clock.now();
    let color = req.color.as_deref().unwrap_or("#3b82f6").trim().to_owned();
    let id: i64 = crate::db::query_scalar(
        "INSERT INTO spending_event \
         (member_id, name, description, start_date, end_date, budget_amount, color, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8) RETURNING id",
    )
    .bind(auth.member_id)
    .bind(req.name.trim())
    .bind(req.description.as_deref().map(str::trim).filter(|s| !s.is_empty()))
    .bind(req.start_date)
    .bind(req.end_date)
    .bind(req.budget_amount)
    .bind(&color)
    .bind(now)
    .fetch_one(&state.db)
    .await?;
    Ok((StatusCode::CREATED, axum::Json(serde_json::json!({ "id": id }))))
}

async fn update_event(
    State(state): State<AppState>,
    auth: Auth,
    AxumPath(id): AxumPath<i64>,
    Json(req): Json<UpsertEventRequest>,
) -> AppResult<StatusCode> {
    let now = state.clock.now();
    let color = req.color.as_deref().unwrap_or("#3b82f6").trim().to_owned();
    let affected = crate::db::query(
        "UPDATE spending_event \
         SET name = $1, description = $2, start_date = $3, end_date = $4, \
             budget_amount = $5, color = $6, updated_at = $7 \
         WHERE id = $8 AND member_id = $9",
    )
    .bind(req.name.trim())
    .bind(req.description.as_deref().map(str::trim).filter(|s| !s.is_empty()))
    .bind(req.start_date)
    .bind(req.end_date)
    .bind(req.budget_amount)
    .bind(&color)
    .bind(now)
    .bind(id)
    .bind(auth.member_id)
    .execute(&state.db)
    .await?
    .rows_affected();
    if affected == 0 {
        return Err(AppError::NotFound("Spending event not found".to_owned()));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_event(
    State(state): State<AppState>,
    auth: Auth,
    AxumPath(id): AxumPath<i64>,
) -> AppResult<StatusCode> {
    let affected = crate::db::query(
        "DELETE FROM spending_event WHERE id = $1 AND member_id = $2",
    )
    .bind(id)
    .bind(auth.member_id)
    .execute(&state.db)
    .await?
    .rows_affected();
    if affected == 0 {
        return Err(AppError::NotFound("Spending event not found".to_owned()));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn link_transaction(
    State(state): State<AppState>,
    auth: Auth,
    AxumPath((event_id, tx_id)): AxumPath<(i64, i64)>,
) -> AppResult<StatusCode> {
    let ev_ok: Option<i64> = crate::db::query_scalar(
        "SELECT id FROM spending_event WHERE id = $1 AND member_id = $2",
    )
    .bind(event_id)
    .bind(auth.member_id)
    .fetch_optional(&state.db)
    .await?;
    if ev_ok.is_none() {
        return Err(AppError::NotFound("Spending event not found".to_owned()));
    }
    let tx_ok: Option<i64> = crate::db::query_scalar(
        "SELECT t.id FROM transaction t JOIN account a ON a.id = t.account_id \
         WHERE t.id = $1 AND a.member_id = $2 AND a.deleted_at IS NULL",
    )
    .bind(tx_id)
    .bind(auth.member_id)
    .fetch_optional(&state.db)
    .await?;
    if tx_ok.is_none() {
        return Err(AppError::NotFound("Transaction not found".to_owned()));
    }
    crate::db::query(
        "INSERT INTO spending_event_transaction (event_id, transaction_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(event_id)
    .bind(tx_id)
    .execute(&state.db)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn unlink_transaction(
    State(state): State<AppState>,
    auth: Auth,
    AxumPath((event_id, tx_id)): AxumPath<(i64, i64)>,
) -> AppResult<StatusCode> {
    let ev_ok: Option<i64> = crate::db::query_scalar(
        "SELECT id FROM spending_event WHERE id = $1 AND member_id = $2",
    )
    .bind(event_id)
    .bind(auth.member_id)
    .fetch_optional(&state.db)
    .await?;
    if ev_ok.is_none() {
        return Err(AppError::NotFound("Spending event not found".to_owned()));
    }
    crate::db::query(
        "DELETE FROM spending_event_transaction WHERE event_id = $1 AND transaction_id = $2",
    )
    .bind(event_id)
    .bind(tx_id)
    .execute(&state.db)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
