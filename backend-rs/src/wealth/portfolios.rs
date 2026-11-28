//! Virtual Portfolios: custom named groupings of arbitrary accounts independent of family members.

use std::collections::HashMap;

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
        .get("/api/portfolios", list_portfolios)
        .post("/api/portfolios", create_portfolio)
        .put("/api/portfolios/{id}", update_portfolio)
        .delete("/api/portfolios/{id}", delete_portfolio);
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VirtualPortfolioAccountView {
    pub id: i64,
    pub name: String,
    pub account_type: String,
    pub balance_eur: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VirtualPortfolioView {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub color: String,
    pub total_value_eur: f64,
    pub account_count: usize,
    pub accounts: Vec<VirtualPortfolioAccountView>,
}

#[derive(DbFromRow)]
struct PortfolioRow {
    id: i64,
    name: String,
    description: Option<String>,
    color: String,
}

#[derive(DbFromRow)]
struct PortfolioAccountRow {
    portfolio_id: i64,
    account_id: i64,
    account_name: String,
    account_type: String,
    current_balance: Decimal,
}

async fn list_portfolios(
    State(state): State<AppState>,
    auth: Auth,
) -> AppResult<axum::Json<Vec<VirtualPortfolioView>>> {
    let rows: Vec<PortfolioRow> = crate::db::query_as(
        "SELECT id, name, description, color FROM virtual_portfolio \
         WHERE member_id = $1 ORDER BY id",
    )
    .bind(auth.member_id)
    .fetch_all(&state.db)
    .await?;

    if rows.is_empty() {
        return Ok(axum::Json(Vec::new()));
    }

    let linked: Vec<PortfolioAccountRow> = crate::db::query_as(
        "SELECT vpa.portfolio_id, a.id AS account_id, a.name AS account_name, \
                a.type::text AS account_type, a.current_balance \
         FROM virtual_portfolio_account vpa \
         JOIN virtual_portfolio vp ON vp.id = vpa.portfolio_id \
         JOIN account a ON a.id = vpa.account_id \
         WHERE vp.member_id = $1 AND a.deleted_at IS NULL \
         ORDER BY a.name",
    )
    .bind(auth.member_id)
    .fetch_all(&state.db)
    .await?;

    let mut by_portfolio: HashMap<i64, Vec<VirtualPortfolioAccountView>> = HashMap::new();
    for item in linked {
        let bal = item.current_balance.to_f64().unwrap_or(0.0);
        let signed = if item.account_type == "LOAN" { -bal.abs() } else { bal };
        by_portfolio
            .entry(item.portfolio_id)
            .or_default()
            .push(VirtualPortfolioAccountView {
                id: item.account_id,
                name: item.account_name,
                account_type: item.account_type,
                balance_eur: (signed * 100.0).round() / 100.0,
            });
    }

    let out = rows
        .into_iter()
        .map(|p| {
            let accounts = by_portfolio.remove(&p.id).unwrap_or_default();
            let total: f64 = accounts.iter().map(|a| a.balance_eur).sum();
            let account_count = accounts.len();
            VirtualPortfolioView {
                id: p.id,
                name: p.name,
                description: p.description,
                color: p.color,
                total_value_eur: (total * 100.0).round() / 100.0,
                account_count,
                accounts,
            }
        })
        .collect();

    Ok(axum::Json(out))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpsertPortfolioRequest {
    name: String,
    description: Option<String>,
    color: Option<String>,
    account_ids: Vec<i64>,
}

impl Validate for UpsertPortfolioRequest {
    fn validate(&self, errors: &mut Errors) {
        if self.name.trim().is_empty() || self.name.chars().count() > 120 {
            errors.add("name", "Name must be between 1 and 120 characters");
        }
    }
}

async fn filter_owned(state: &AppState, member_id: i64, ids: &[i64]) -> AppResult<Vec<i64>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(crate::db::query_scalar(
        "SELECT id FROM account WHERE member_id = $1 AND deleted_at IS NULL AND id = ANY($2)",
    )
    .bind(member_id)
    .bind(ids)
    .fetch_all(&state.db)
    .await?)
}

async fn create_portfolio(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<UpsertPortfolioRequest>,
) -> AppResult<(StatusCode, axum::Json<serde_json::Value>)> {
    let now = state.clock.now();
    let color = req.color.as_deref().unwrap_or("#3b82f6").trim().to_owned();
    let owned = filter_owned(&state, auth.member_id, &req.account_ids).await?;

    let id: i64 = crate::db::query_scalar(
        "INSERT INTO virtual_portfolio (member_id, name, description, color, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $5) RETURNING id",
    )
    .bind(auth.member_id)
    .bind(req.name.trim())
    .bind(req.description.as_deref().map(str::trim).filter(|s| !s.is_empty()))
    .bind(&color)
    .bind(now)
    .fetch_one(&state.db)
    .await?;

    for acc_id in owned {
        crate::db::query(
            "INSERT INTO virtual_portfolio_account (portfolio_id, account_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(acc_id)
        .execute(&state.db)
        .await?;
    }

    Ok((StatusCode::CREATED, axum::Json(serde_json::json!({ "id": id }))))
}

async fn update_portfolio(
    State(state): State<AppState>,
    auth: Auth,
    AxumPath(id): AxumPath<i64>,
    Json(req): Json<UpsertPortfolioRequest>,
) -> AppResult<StatusCode> {
    let now = state.clock.now();
    let color = req.color.as_deref().unwrap_or("#3b82f6").trim().to_owned();
    let owned = filter_owned(&state, auth.member_id, &req.account_ids).await?;

    let affected = crate::db::query(
        "UPDATE virtual_portfolio SET name = $1, description = $2, color = $3, updated_at = $4 \
         WHERE id = $5 AND member_id = $6",
    )
    .bind(req.name.trim())
    .bind(req.description.as_deref().map(str::trim).filter(|s| !s.is_empty()))
    .bind(&color)
    .bind(now)
    .bind(id)
    .bind(auth.member_id)
    .execute(&state.db)
    .await?
    .rows_affected();

    if affected == 0 {
        return Err(AppError::NotFound("Virtual portfolio not found".to_owned()));
    }

    crate::db::query("DELETE FROM virtual_portfolio_account WHERE portfolio_id = $1")
        .bind(id)
        .execute(&state.db)
        .await?;

    for acc_id in owned {
        crate::db::query(
            "INSERT INTO virtual_portfolio_account (portfolio_id, account_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(acc_id)
        .execute(&state.db)
        .await?;
    }

    Ok(StatusCode::NO_CONTENT)
}

async fn delete_portfolio(
    State(state): State<AppState>,
    auth: Auth,
    AxumPath(id): AxumPath<i64>,
) -> AppResult<StatusCode> {
    let affected = crate::db::query(
        "DELETE FROM virtual_portfolio WHERE id = $1 AND member_id = $2",
    )
    .bind(id)
    .bind(auth.member_id)
    .execute(&state.db)
    .await?
    .rows_affected();
    if affected == 0 {
        return Err(AppError::NotFound("Virtual portfolio not found".to_owned()));
    }
    Ok(StatusCode::NO_CONTENT)
}
