//! Contribution Limits: track lifetime and annual deposit caps across tax-advantaged envelopes
//! (French PEA, PEA-PME, Livret A, LDDS, LEP, Livret Jeune, PEL, PER, or custom envelopes).

use std::collections::HashMap;

use axum::extract::{Path as AxumPath, State};
use chrono::Datelike;
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
        .get("/api/contribution-limits", list_limits)
        .get("/api/contribution-limits/presets", list_presets)
        .post("/api/contribution-limits", create_limit)
        .put("/api/contribution-limits/{id}", update_limit)
        .delete("/api/contribution-limits/{id}", delete_limit);
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitPreset {
    pub envelope_kind: &'static str,
    pub label: &'static str,
    pub limit_amount: f64,
    pub period_kind: &'static str,
    pub matching_account_types: &'static [&'static str],
}

pub const FRENCH_LIMIT_PRESETS: &[LimitPreset] = &[
    LimitPreset {
        envelope_kind: "PEA",
        label: "PEA",
        limit_amount: 150_000.0,
        period_kind: "LIFETIME",
        matching_account_types: &["PEA"],
    },
    LimitPreset {
        envelope_kind: "PEA_PME",
        label: "PEA-PME",
        limit_amount: 225_000.0,
        period_kind: "LIFETIME",
        matching_account_types: &["PEA"],
    },
    LimitPreset {
        envelope_kind: "LIVRET_A",
        label: "Livret A",
        limit_amount: 22_950.0,
        period_kind: "LIFETIME",
        matching_account_types: &["LIVRET_A"],
    },
    LimitPreset {
        envelope_kind: "LDDS",
        label: "LDDS",
        limit_amount: 12_000.0,
        period_kind: "LIFETIME",
        matching_account_types: &["LDDS"],
    },
    LimitPreset {
        envelope_kind: "LEP",
        label: "LEP",
        limit_amount: 10_000.0,
        period_kind: "LIFETIME",
        matching_account_types: &["LEP"],
    },
    LimitPreset {
        envelope_kind: "LIVRET_JEUNE",
        label: "Livret Jeune",
        limit_amount: 1_600.0,
        period_kind: "LIFETIME",
        matching_account_types: &["LIVRET_JEUNE"],
    },
    LimitPreset {
        envelope_kind: "PEL",
        label: "PEL",
        limit_amount: 61_200.0,
        period_kind: "LIFETIME",
        matching_account_types: &["PEL"],
    },
    LimitPreset {
        envelope_kind: "PER",
        label: "PER (plafond annuel)",
        limit_amount: 37_094.0,
        period_kind: "ANNUAL",
        matching_account_types: &["PER"],
    },
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitLinkedAccount {
    pub id: i64,
    pub name: String,
    pub account_type: String,
    pub contributed_amount: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContributionLimitView {
    pub id: i64,
    pub name: String,
    pub envelope_kind: String,
    pub limit_amount: f64,
    pub period_kind: String,
    pub period_year: Option<i32>,
    pub currency: String,
    pub contributed_amount: f64,
    pub remaining_amount: f64,
    pub usage_percent: f64,
    pub status: &'static str,
    pub accounts: Vec<LimitLinkedAccount>,
}

pub fn evaluate_limit_status(contributed: f64, limit: f64) -> (f64, f64, &'static str) {
    let remaining = (limit - contributed).max(0.0);
    let usage = if limit > 0.0 {
        ((contributed / limit) * 1000.0).round() / 10.0
    } else {
        0.0
    };
    let status = if contributed > limit {
        "EXCEEDED"
    } else if usage >= 85.0 {
        "NEAR_LIMIT"
    } else {
        "OK"
    };
    (remaining, usage, status)
}

#[derive(DbFromRow)]
struct LimitRow {
    id: i64,
    name: String,
    envelope_kind: String,
    limit_amount: Decimal,
    period_kind: String,
    period_year: Option<i32>,
    currency: String,
}

#[derive(DbFromRow)]
struct LinkedAccountRow {
    limit_id: i64,
    account_id: i64,
    account_name: String,
    account_type: String,
    current_balance: Decimal,
    provider_paid_in_eur: Option<Decimal>,
    provider_invested_eur: Option<Decimal>,
}

#[derive(DbFromRow)]
struct DepositSumRow {
    account_id: i64,
    total: Decimal,
}

async fn list_presets(_auth: Auth) -> axum::Json<&'static [LimitPreset]> {
    axum::Json(FRENCH_LIMIT_PRESETS)
}

async fn list_limits(
    State(state): State<AppState>,
    auth: Auth,
) -> AppResult<axum::Json<Vec<ContributionLimitView>>> {
    let current_year = state.clock.today().year();
    let limits: Vec<LimitRow> = crate::db::query_as(
        "SELECT id, name, envelope_kind, limit_amount, period_kind, period_year, currency \
         FROM contribution_limit WHERE member_id = $1 ORDER BY id",
    )
    .bind(auth.member_id)
    .fetch_all(&state.db)
    .await?;

    if limits.is_empty() {
        return Ok(axum::Json(Vec::new()));
    }

    let linked: Vec<LinkedAccountRow> = crate::db::query_as(
        "SELECT cla.limit_id, a.id AS account_id, a.name AS account_name, a.type::text AS account_type, \
                a.current_balance, a.provider_paid_in_eur, a.provider_invested_eur \
         FROM contribution_limit_account cla \
         JOIN contribution_limit cl ON cl.id = cla.limit_id \
         JOIN account a ON a.id = cla.account_id \
         WHERE cl.member_id = $1 AND a.deleted_at IS NULL \
         ORDER BY a.name",
    )
    .bind(auth.member_id)
    .fetch_all(&state.db)
    .await?;

    // Compute lifetime deposit totals per linked account from transactions.
    let lifetime_tx: HashMap<i64, Decimal> = crate::db::query_as::<DepositSumRow>(
        "SELECT t.account_id, COALESCE(SUM(ABS(t.amount)), 0) AS total \
         FROM transaction t \
         JOIN account a ON a.id = t.account_id \
         WHERE a.member_id = $1 AND a.deleted_at IS NULL \
           AND (t.tx_type = 'DEPOSIT' OR (t.tx_type IS NULL AND t.amount > 0)) \
         GROUP BY t.account_id",
    )
    .bind(auth.member_id)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(|r| (r.account_id, r.total))
    .collect();

    let mut by_limit: HashMap<i64, Vec<&LinkedAccountRow>> = HashMap::new();
    for row in &linked {
        by_limit.entry(row.limit_id).or_default().push(row);
    }

    let mut out = Vec::with_capacity(limits.len());
    for lim in limits {
        let year = lim.period_year.unwrap_or(current_year);
        let annual_tx: HashMap<i64, Decimal> = if lim.period_kind == "ANNUAL" {
            let start = chrono::NaiveDate::from_ymd_opt(year, 1, 1).unwrap_or(state.clock.today());
            let end = chrono::NaiveDate::from_ymd_opt(year, 12, 31).unwrap_or(state.clock.today());
            crate::db::query_as::<DepositSumRow>(
                "SELECT t.account_id, COALESCE(SUM(ABS(t.amount)), 0) AS total \
                 FROM transaction t \
                 JOIN account a ON a.id = t.account_id \
                 WHERE a.member_id = $1 AND a.deleted_at IS NULL \
                   AND t.date >= $2 AND t.date <= $3 \
                   AND (t.tx_type = 'DEPOSIT' OR (t.tx_type IS NULL AND t.amount > 0)) \
                 GROUP BY t.account_id",
            )
            .bind(auth.member_id)
            .bind(start)
            .bind(end)
            .fetch_all(&state.db)
            .await?
            .into_iter()
            .map(|r| (r.account_id, r.total))
            .collect()
        } else {
            HashMap::new()
        };

        let mut accounts_view = Vec::new();
        let mut contributed = 0.0;
        for acc in by_limit.remove(&lim.id).unwrap_or_default() {
            let amount = if lim.period_kind == "ANNUAL" {
                annual_tx
                    .get(&acc.account_id)
                    .and_then(|d| d.to_f64())
                    .unwrap_or(0.0)
            } else if let Some(paid_in) = acc.provider_paid_in_eur.and_then(|d| d.to_f64()) {
                paid_in.max(0.0)
            } else if let Some(invested) = acc.provider_invested_eur.and_then(|d| d.to_f64()) {
                invested.max(0.0)
            } else if let Some(tx_sum) = lifetime_tx
                .get(&acc.account_id)
                .and_then(|d| d.to_f64())
                .filter(|v| *v > 0.0)
            {
                tx_sum
            } else {
                acc.current_balance.to_f64().unwrap_or(0.0).max(0.0)
            };
            contributed += amount;
            accounts_view.push(LimitLinkedAccount {
                id: acc.account_id,
                name: acc.account_name.clone(),
                account_type: acc.account_type.clone(),
                contributed_amount: (amount * 100.0).round() / 100.0,
            });
        }

        let limit_amount = lim.limit_amount.to_f64().unwrap_or(0.0);
        let contributed_rounded = (contributed * 100.0).round() / 100.0;
        let (remaining, usage, status) = evaluate_limit_status(contributed_rounded, limit_amount);

        out.push(ContributionLimitView {
            id: lim.id,
            name: lim.name,
            envelope_kind: lim.envelope_kind,
            limit_amount,
            period_kind: lim.period_kind,
            period_year: lim.period_year,
            currency: lim.currency,
            contributed_amount: contributed_rounded,
            remaining_amount: (remaining * 100.0).round() / 100.0,
            usage_percent: usage,
            status,
            accounts: accounts_view,
        });
    }

    Ok(axum::Json(out))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpsertLimitRequest {
    name: String,
    envelope_kind: Option<String>,
    limit_amount: Decimal,
    period_kind: Option<String>,
    period_year: Option<i32>,
    currency: Option<String>,
    account_ids: Vec<i64>,
}

impl Validate for UpsertLimitRequest {
    fn validate(&self, errors: &mut Errors) {
        if self.name.trim().is_empty() || self.name.chars().count() > 120 {
            errors.add("name", "Name must be between 1 and 120 characters");
        }
        if self.limit_amount <= Decimal::ZERO {
            errors.add("limitAmount", "Limit amount must be strictly positive");
        }
        if let Some(pk) = &self.period_kind
            && pk != "ANNUAL"
            && pk != "LIFETIME"
        {
            errors.add("periodKind", "periodKind must be ANNUAL or LIFETIME");
        }
    }
}

async fn verify_owned_accounts(
    state: &AppState,
    member_id: i64,
    account_ids: &[i64],
) -> AppResult<Vec<i64>> {
    if account_ids.is_empty() {
        return Ok(Vec::new());
    }
    let valid: Vec<i64> = crate::db::query_scalar(
        "SELECT id FROM account WHERE member_id = $1 AND deleted_at IS NULL AND id = ANY($2)",
    )
    .bind(member_id)
    .bind(account_ids)
    .fetch_all(&state.db)
    .await?;
    Ok(valid)
}

async fn create_limit(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<UpsertLimitRequest>,
) -> AppResult<(StatusCode, axum::Json<serde_json::Value>)> {
    let now = state.clock.now();
    let envelope = req
        .envelope_kind
        .as_deref()
        .unwrap_or("CUSTOM")
        .trim()
        .to_uppercase();
    let period = req
        .period_kind
        .as_deref()
        .unwrap_or("LIFETIME")
        .to_owned();
    let currency = req
        .currency
        .as_deref()
        .unwrap_or("EUR")
        .trim()
        .to_uppercase();
    let owned_accounts = verify_owned_accounts(&state, auth.member_id, &req.account_ids).await?;

    let id: i64 = crate::db::query_scalar(
        "INSERT INTO contribution_limit \
         (member_id, name, envelope_kind, limit_amount, period_kind, period_year, currency, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8) RETURNING id",
    )
    .bind(auth.member_id)
    .bind(req.name.trim())
    .bind(&envelope)
    .bind(req.limit_amount)
    .bind(&period)
    .bind(req.period_year)
    .bind(&currency)
    .bind(now)
    .fetch_one(&state.db)
    .await?;

    for account_id in owned_accounts {
        crate::db::query(
            "INSERT INTO contribution_limit_account (limit_id, account_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(account_id)
        .execute(&state.db)
        .await?;
    }

    Ok((StatusCode::CREATED, axum::Json(serde_json::json!({ "id": id }))))
}

async fn update_limit(
    State(state): State<AppState>,
    auth: Auth,
    AxumPath(id): AxumPath<i64>,
    Json(req): Json<UpsertLimitRequest>,
) -> AppResult<StatusCode> {
    let now = state.clock.now();
    let envelope = req
        .envelope_kind
        .as_deref()
        .unwrap_or("CUSTOM")
        .trim()
        .to_uppercase();
    let period = req
        .period_kind
        .as_deref()
        .unwrap_or("LIFETIME")
        .to_owned();
    let currency = req
        .currency
        .as_deref()
        .unwrap_or("EUR")
        .trim()
        .to_uppercase();
    let owned_accounts = verify_owned_accounts(&state, auth.member_id, &req.account_ids).await?;

    let affected = crate::db::query(
        "UPDATE contribution_limit \
         SET name = $1, envelope_kind = $2, limit_amount = $3, period_kind = $4, \
             period_year = $5, currency = $6, updated_at = $7 \
         WHERE id = $8 AND member_id = $9",
    )
    .bind(req.name.trim())
    .bind(&envelope)
    .bind(req.limit_amount)
    .bind(&period)
    .bind(req.period_year)
    .bind(&currency)
    .bind(now)
    .bind(id)
    .bind(auth.member_id)
    .execute(&state.db)
    .await?
    .rows_affected();

    if affected == 0 {
        return Err(AppError::NotFound("Contribution limit not found".to_owned()));
    }

    crate::db::query("DELETE FROM contribution_limit_account WHERE limit_id = $1")
        .bind(id)
        .execute(&state.db)
        .await?;

    for account_id in owned_accounts {
        crate::db::query(
            "INSERT INTO contribution_limit_account (limit_id, account_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(account_id)
        .execute(&state.db)
        .await?;
    }

    Ok(StatusCode::NO_CONTENT)
}

async fn delete_limit(
    State(state): State<AppState>,
    auth: Auth,
    AxumPath(id): AxumPath<i64>,
) -> AppResult<StatusCode> {
    let affected = crate::db::query(
        "DELETE FROM contribution_limit WHERE id = $1 AND member_id = $2",
    )
    .bind(id)
    .bind(auth.member_id)
    .execute(&state.db)
    .await?
    .rows_affected();
    if affected == 0 {
        return Err(AppError::NotFound("Contribution limit not found".to_owned()));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluates_ok_near_limit_and_exceeded_correctly() {
        assert_eq!(evaluate_limit_status(50_000.0, 150_000.0), (100_000.0, 33.3, "OK"));
        assert_eq!(evaluate_limit_status(135_000.0, 150_000.0), (15_000.0, 90.0, "NEAR_LIMIT"));
        assert_eq!(evaluate_limit_status(23_500.0, 22_950.0), (0.0, 102.4, "EXCEEDED"));
    }

    #[test]
    fn french_presets_include_pea_livret_a_ldds_lep_per() {
        let kinds: Vec<_> = FRENCH_LIMIT_PRESETS.iter().map(|p| p.envelope_kind).collect();
        assert!(kinds.contains(&"PEA"));
        assert!(kinds.contains(&"LIVRET_A"));
        assert!(kinds.contains(&"LDDS"));
        assert!(kinds.contains(&"LEP"));
        assert!(kinds.contains(&"PER"));
    }
}
