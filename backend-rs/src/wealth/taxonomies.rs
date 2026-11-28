//! Custom user-defined taxonomies: create custom classification axes (e.g. Liquidity, Risk,
//! Tax wrapper, ESG), define categories with optional target %, assign holdings, and view live
//! EUR breakdown.

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
        .get("/api/taxonomies", list_taxonomies)
        .post("/api/taxonomies", create_taxonomy)
        .delete("/api/taxonomies/{id}", delete_taxonomy)
        .post("/api/taxonomies/{id}/categories", create_category)
        .post("/api/taxonomies/{id}/assignments", assign_holding);
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaxonomyCategoryView {
    pub id: i64,
    pub name: String,
    pub color: String,
    pub target_percent: Option<f64>,
    pub allocated_value_eur: f64,
    pub actual_percent: f64,
    pub assigned_tickers: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomTaxonomyView {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub total_classified_eur: f64,
    pub categories: Vec<TaxonomyCategoryView>,
}

#[derive(DbFromRow)]
struct TaxonomyRow {
    id: i64,
    name: String,
    description: Option<String>,
}

#[derive(DbFromRow)]
struct CategoryRow {
    id: i64,
    taxonomy_id: i64,
    name: String,
    color: String,
    target_percent: Option<Decimal>,
}

#[derive(DbFromRow)]
struct AssignmentRow {
    taxonomy_id: i64,
    category_id: i64,
    ticker: String,
    weight_percent: Decimal,
}

#[derive(DbFromRow)]
struct HoldingValRow {
    ticker: String,
    quantity: Decimal,
    current_price: Option<Decimal>,
    provider_value_eur: Option<Decimal>,
}

async fn list_taxonomies(
    State(state): State<AppState>,
    auth: Auth,
) -> AppResult<axum::Json<Vec<CustomTaxonomyView>>> {
    let taxonomies: Vec<TaxonomyRow> = crate::db::query_as(
        "SELECT id, name, description FROM custom_taxonomy WHERE member_id = $1 ORDER BY id",
    )
    .bind(auth.member_id)
    .fetch_all(&state.db)
    .await?;

    if taxonomies.is_empty() {
        return Ok(axum::Json(Vec::new()));
    }

    let categories: Vec<CategoryRow> = crate::db::query_as(
        "SELECT c.id, c.taxonomy_id, c.name, c.color, c.target_percent \
         FROM custom_taxonomy_category c \
         JOIN custom_taxonomy t ON t.id = c.taxonomy_id \
         WHERE t.member_id = $1 ORDER BY c.id",
    )
    .bind(auth.member_id)
    .fetch_all(&state.db)
    .await?;

    let assignments: Vec<AssignmentRow> = crate::db::query_as(
        "SELECT taxonomy_id, category_id, ticker, weight_percent \
         FROM holding_taxonomy_assignment WHERE member_id = $1",
    )
    .bind(auth.member_id)
    .fetch_all(&state.db)
    .await?;

    let holdings: Vec<HoldingValRow> = crate::db::query_as(
        "SELECT h.ticker, h.quantity, h.current_price, h.provider_value_eur \
         FROM account_holding h \
         JOIN account a ON a.id = h.account_id \
         WHERE a.member_id = $1 AND a.deleted_at IS NULL AND a.hidden = false",
    )
    .bind(auth.member_id)
    .fetch_all(&state.db)
    .await?;

    let mut ticker_values: HashMap<String, f64> = HashMap::new();
    for h in holdings {
        let val = h
            .provider_value_eur
            .and_then(|d| d.to_f64())
            .or_else(|| h.current_price.and_then(|p| (p * h.quantity).to_f64()))
            .unwrap_or(0.0)
            .max(0.0);
        *ticker_values.entry(h.ticker.trim().to_uppercase()).or_insert(0.0) += val;
    }

    let mut cats_by_tax: HashMap<i64, Vec<CategoryRow>> = HashMap::new();
    for c in categories {
        cats_by_tax.entry(c.taxonomy_id).or_default().push(c);
    }

    let mut assigns_by_cat: HashMap<i64, Vec<(String, f64)>> = HashMap::new();
    for a in assignments {
        let _ = a.taxonomy_id;
        assigns_by_cat
            .entry(a.category_id)
            .or_default()
            .push((a.ticker, a.weight_percent.to_f64().unwrap_or(100.0)));
    }

    let mut out = Vec::with_capacity(taxonomies.len());
    for tax in taxonomies {
        let raw_cats = cats_by_tax.remove(&tax.id).unwrap_or_default();
        let mut temp_cats: Vec<(CategoryRow, f64, Vec<String>)> = Vec::with_capacity(raw_cats.len());
        let mut total_classified = 0.0_f64;

        for c in raw_cats {
            let assigned = assigns_by_cat.remove(&c.id).unwrap_or_default();
            let mut cat_val = 0.0_f64;
            let mut tickers = Vec::with_capacity(assigned.len());
            for (tk, weight) in assigned {
                let v = ticker_values
                    .get(&tk.trim().to_uppercase())
                    .copied()
                    .unwrap_or(0.0);
                cat_val += v * (weight / 100.0);
                tickers.push(tk);
            }
            total_classified += cat_val;
            temp_cats.push((c, cat_val, tickers));
        }

        let cat_views = temp_cats
            .into_iter()
            .map(|(c, val, tickers)| {
                let pct = if total_classified > 0.0 {
                    ((val / total_classified) * 1000.0).round() / 10.0
                } else {
                    0.0
                };
                TaxonomyCategoryView {
                    id: c.id,
                    name: c.name,
                    color: c.color,
                    target_percent: c.target_percent.and_then(|d| d.to_f64()),
                    allocated_value_eur: (val * 100.0).round() / 100.0,
                    actual_percent: pct,
                    assigned_tickers: tickers,
                }
            })
            .collect();

        out.push(CustomTaxonomyView {
            id: tax.id,
            name: tax.name,
            description: tax.description,
            total_classified_eur: (total_classified * 100.0).round() / 100.0,
            categories: cat_views,
        });
    }

    Ok(axum::Json(out))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateTaxonomyRequest {
    name: String,
    description: Option<String>,
}

impl Validate for CreateTaxonomyRequest {
    fn validate(&self, errors: &mut Errors) {
        if self.name.trim().is_empty() || self.name.chars().count() > 120 {
            errors.add("name", "Name must be between 1 and 120 characters");
        }
    }
}

async fn create_taxonomy(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<CreateTaxonomyRequest>,
) -> AppResult<(StatusCode, axum::Json<serde_json::Value>)> {
    let now = state.clock.now();
    let id: i64 = crate::db::query_scalar(
        "INSERT INTO custom_taxonomy (member_id, name, description, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $4) RETURNING id",
    )
    .bind(auth.member_id)
    .bind(req.name.trim())
    .bind(req.description.as_deref().map(str::trim).filter(|s| !s.is_empty()))
    .bind(now)
    .fetch_one(&state.db)
    .await?;
    Ok((StatusCode::CREATED, axum::Json(serde_json::json!({ "id": id }))))
}

async fn delete_taxonomy(
    State(state): State<AppState>,
    auth: Auth,
    AxumPath(id): AxumPath<i64>,
) -> AppResult<StatusCode> {
    let affected = crate::db::query(
        "DELETE FROM custom_taxonomy WHERE id = $1 AND member_id = $2",
    )
    .bind(id)
    .bind(auth.member_id)
    .execute(&state.db)
    .await?
    .rows_affected();
    if affected == 0 {
        return Err(AppError::NotFound("Taxonomy not found".to_owned()));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateCategoryRequest {
    name: String,
    color: Option<String>,
    target_percent: Option<Decimal>,
}

impl Validate for CreateCategoryRequest {
    fn validate(&self, errors: &mut Errors) {
        if self.name.trim().is_empty() || self.name.chars().count() > 120 {
            errors.add("name", "Name is required");
        }
    }
}

async fn create_category(
    State(state): State<AppState>,
    auth: Auth,
    AxumPath(taxonomy_id): AxumPath<i64>,
    Json(req): Json<CreateCategoryRequest>,
) -> AppResult<(StatusCode, axum::Json<serde_json::Value>)> {
    let exists: Option<i64> = crate::db::query_scalar(
        "SELECT id FROM custom_taxonomy WHERE id = $1 AND member_id = $2",
    )
    .bind(taxonomy_id)
    .bind(auth.member_id)
    .fetch_optional(&state.db)
    .await?;
    if exists.is_none() {
        return Err(AppError::NotFound("Taxonomy not found".to_owned()));
    }
    let color = req.color.as_deref().unwrap_or("#6366f1").trim().to_owned();
    let id: i64 = crate::db::query_scalar(
        "INSERT INTO custom_taxonomy_category (taxonomy_id, name, color, target_percent, created_at) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(taxonomy_id)
    .bind(req.name.trim())
    .bind(&color)
    .bind(req.target_percent)
    .bind(state.clock.now())
    .fetch_one(&state.db)
    .await?;
    Ok((StatusCode::CREATED, axum::Json(serde_json::json!({ "id": id }))))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssignHoldingRequest {
    category_id: i64,
    ticker: String,
    weight_percent: Option<Decimal>,
}

impl Validate for AssignHoldingRequest {
    fn validate(&self, errors: &mut Errors) {
        if self.ticker.trim().is_empty() {
            errors.add("ticker", "Ticker is required");
        }
    }
}

async fn assign_holding(
    State(state): State<AppState>,
    auth: Auth,
    AxumPath(taxonomy_id): AxumPath<i64>,
    Json(req): Json<AssignHoldingRequest>,
) -> AppResult<StatusCode> {
    let cat_ok: Option<i64> = crate::db::query_scalar(
        "SELECT c.id FROM custom_taxonomy_category c \
         JOIN custom_taxonomy t ON t.id = c.taxonomy_id \
         WHERE c.id = $1 AND t.id = $2 AND t.member_id = $3",
    )
    .bind(req.category_id)
    .bind(taxonomy_id)
    .bind(auth.member_id)
    .fetch_optional(&state.db)
    .await?;
    if cat_ok.is_none() {
        return Err(AppError::NotFound("Taxonomy category not found".to_owned()));
    }
    let weight = req.weight_percent.unwrap_or(Decimal::from(100));
    crate::db::query(
        "INSERT INTO holding_taxonomy_assignment \
         (member_id, taxonomy_id, category_id, ticker, weight_percent, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (member_id, taxonomy_id, category_id, ticker) \
         DO UPDATE SET weight_percent = EXCLUDED.weight_percent",
    )
    .bind(auth.member_id)
    .bind(taxonomy_id)
    .bind(req.category_id)
    .bind(req.ticker.trim().to_uppercase())
    .bind(weight)
    .bind(state.clock.now())
    .execute(&state.db)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
