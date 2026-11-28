//! HoldingClassificationService: the member's own verdict on what a holding is (tier, sector,
//! country), keyed on (member, ticker) so it outlives resyncs and covers every account.

use std::collections::HashMap;
use std::sync::LazyLock;

use axum::extract::State;
use regex::Regex;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::accounts::analysis::profiles;
use crate::auth::Auth;
use crate::db::{self, AsDbHandle, DbExec, DbFromRow};
use crate::error::AppResult;
use crate::http::json::{Errors, Json, Path, Validate};
use crate::ledger::access;
use crate::ledger::model::WealthTier;
use crate::ledger::sync_values::java_trim;
use crate::routes::Routes;

pub fn register(routes: &mut Routes) {
    routes
        .get("/api/accounts/{id}/holdings/{ticker}/classification", view)
        .put(
            "/api/accounts/{id}/holdings/{ticker}/classification",
            classify,
        );
}

/// A holding_classification row.
#[derive(Debug, Clone, PartialEq, DbFromRow)]
pub struct HoldingClassification {
    pub id: i64,
    pub ticker: String,
    pub wealth_tier: Option<WealthTier>,
    pub sector_key: Option<String>,
    pub country_key: Option<String>,
    pub equity_pct: Option<Decimal>,
    pub bonds_pct: Option<Decimal>,
    pub cash_pct: Option<Decimal>,
}

impl HoldingClassification {
    /// Returns `(equity_pct, bonds_pct, cash_pct)` when the member declared a multi-asset
    /// complex product split (summing to 100%).
    pub fn complex_mix(&self) -> Option<(Decimal, Decimal, Decimal)> {
        match (self.equity_pct, self.bonds_pct, self.cash_pct) {
            (Some(e), Some(b), Some(c)) => Some((e, b, c)),
            _ => None,
        }
    }
}

const COLUMNS: &str =
    "id, ticker, wealth_tier, sector_key, country_key, equity_pct, bonds_pct, cash_pct";

/// findByMemberId keyed by upper-cased ticker, as every reader looks it up.
pub async fn overrides_of<'e>(
    db: impl DbExec<'e>,
    member_id: i64,
) -> sqlx::Result<HashMap<String, HoldingClassification>> {
    let rows: Vec<HoldingClassification> = db::query_as(&format!(
        "SELECT {COLUMNS} FROM holding_classification WHERE member_id = $1 ORDER BY id"
    ))
    .bind(member_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|c| (c.ticker.to_uppercase(), c))
        .collect())
}

async fn find<'e>(
    db: impl DbExec<'e>,
    member_id: i64,
    ticker: &str,
) -> sqlx::Result<Option<HoldingClassification>> {
    db::query_as(&format!(
        "SELECT {COLUMNS} FROM holding_classification WHERE member_id = $1 AND ticker = $2"
    ))
    .bind(member_id)
    .bind(ticker)
    .fetch_optional(db)
    .await
}

/// HoldingClassificationView: the override in force and, apart, what the providers inferred.
#[serde_with::skip_serializing_none]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HoldingClassificationView {
    ticker: String,
    wealth_tier: Option<WealthTier>,
    sector_key: Option<String>,
    country_key: Option<String>,
    #[serde(with = "crate::accounts::jdecimal::option")]
    equity_pct: Option<Decimal>,
    #[serde(with = "crate::accounts::jdecimal::option")]
    bonds_pct: Option<Decimal>,
    #[serde(with = "crate::accounts::jdecimal::option")]
    cash_pct: Option<Decimal>,
    inferred_sector_key: Option<String>,
    inferred_country_key: Option<String>,
    profile_looked: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HoldingClassificationRequest {
    wealth_tier: Option<WealthTier>,
    sector_key: Option<String>,
    country_key: Option<String>,
    #[serde(default)]
    equity_pct: Option<Decimal>,
    #[serde(default)]
    bonds_pct: Option<Decimal>,
    #[serde(default)]
    cash_pct: Option<Decimal>,
}

static COUNTRY: LazyLock<Regex> = LazyLock::new(|| Regex::new("^[A-Z]{2}$").expect("regex"));

impl Validate for HoldingClassificationRequest {
    fn validate(&self, errors: &mut Errors) {
        errors
            .size("sectorKey", self.sector_key.as_deref(), 0, 40)
            .pattern(
                "countryKey",
                self.country_key.as_deref(),
                &COUNTRY,
                "must be an ISO 3166-1 alpha-2 country or region code",
            );
        let any_mix =
            self.equity_pct.is_some() || self.bonds_pct.is_some() || self.cash_pct.is_some();
        if any_mix {
            match (self.equity_pct, self.bonds_pct, self.cash_pct) {
                (Some(eq), Some(bo), Some(ca)) => {
                    let hundred = Decimal::ONE_HUNDRED;
                    if eq < Decimal::ZERO || eq > hundred {
                        errors.add("equityPct", "must be between 0 and 100");
                    }
                    if bo < Decimal::ZERO || bo > hundred {
                        errors.add("bondsPct", "must be between 0 and 100");
                    }
                    if ca < Decimal::ZERO || ca > hundred {
                        errors.add("cashPct", "must be between 0 and 100");
                    }
                    if eq + bo + ca != hundred {
                        errors.add(
                            "equityPct",
                            "equityPct + bondsPct + cashPct must equal 100",
                        );
                    }
                }
                _ => {
                    errors.add(
                        "equityPct",
                        "equityPct, bondsPct, and cashPct must all be provided together",
                    );
                }
            }
        }
    }
}

/// HoldingClassificationResponse: all fields null means no override is in force.
#[serde_with::skip_serializing_none]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HoldingClassificationResponse {
    ticker: String,
    wealth_tier: Option<WealthTier>,
    sector_key: Option<String>,
    country_key: Option<String>,
    #[serde(with = "crate::accounts::jdecimal::option")]
    equity_pct: Option<Decimal>,
    #[serde(with = "crate::accounts::jdecimal::option")]
    bonds_pct: Option<Decimal>,
    #[serde(with = "crate::accounts::jdecimal::option")]
    cash_pct: Option<Decimal>,
}

/// Readable, not owner-gated: looking at how a shared holding is classified is not a write.
async fn view(
    State(state): State<AppState>,
    auth: Auth,
    Path((id, ticker)): Path<(i64, String)>,
) -> AppResult<axum::Json<HoldingClassificationView>> {
    let mut conn = state.db.pool_handle();
    access::require_readable(&mut conn, id, auth.member_id).await?;
    let upper = java_trim(&ticker).to_uppercase();
    let found = find(&mut conn, auth.member_id, &upper).await?;
    let profile = profiles::load(&mut conn, [upper.as_str()])
        .await?
        .remove(&upper);
    Ok(axum::Json(HoldingClassificationView {
        wealth_tier: found.as_ref().and_then(|c| c.wealth_tier),
        sector_key: found.as_ref().and_then(|c| c.sector_key.clone()),
        country_key: found.as_ref().and_then(|c| c.country_key.clone()),
        equity_pct: found.as_ref().and_then(|c| c.equity_pct),
        bonds_pct: found.as_ref().and_then(|c| c.bonds_pct),
        cash_pct: found.as_ref().and_then(|c| c.cash_pct),
        inferred_sector_key: profile.as_ref().and_then(|p| p.sector_key.clone()),
        inferred_country_key: profile.as_ref().and_then(|p| p.country_key.clone()),
        profile_looked: profile
            .as_ref()
            .is_some_and(profiles::SecurityProfile::was_looked_up),
        ticker: upper,
    }))
}

/// Owner-gated: a co-owner must not rewrite how someone else's holdings are counted.
async fn classify(
    State(state): State<AppState>,
    auth: Auth,
    Path((id, ticker)): Path<(i64, String)>,
    Json(request): Json<HoldingClassificationRequest>,
) -> AppResult<axum::Json<HoldingClassificationResponse>> {
    let mut tx = state.db.begin().await?;
    access::require_owner(&mut tx.as_handle(), id, auth.member_id).await?;
    let upper = java_trim(&ticker).to_uppercase();
    let existing = find(&mut tx, auth.member_id, &upper).await?;
    let has_mix = request.equity_pct.is_some();
    // When a complex mix is declared, it replaces a single wealth tier.
    let effective_tier = if has_mix { None } else { request.wealth_tier };
    if effective_tier.is_none()
        && request.sector_key.is_none()
        && request.country_key.is_none()
        && !has_mix
    {
        // Every field cleared means "stop overriding": drop the row rather than keep a blank.
        if let Some(row) = existing {
            db::query("DELETE FROM holding_classification WHERE id = $1")
                .bind(row.id)
                .execute(&mut tx)
                .await?;
        }
        tx.commit().await?;
        return Ok(axum::Json(HoldingClassificationResponse {
            ticker: upper,
            wealth_tier: None,
            sector_key: None,
            country_key: None,
            equity_pct: None,
            bonds_pct: None,
            cash_pct: None,
        }));
    }
    let now = state.clock.now();
    let saved: HoldingClassification = match existing {
        Some(row) => db::query_as(&format!(
            "UPDATE holding_classification SET ticker = $2, wealth_tier = $3, sector_key = $4, \
             country_key = $5, equity_pct = $6, bonds_pct = $7, cash_pct = $8, updated_at = $9 \
             WHERE id = $1 RETURNING {COLUMNS}"
        ))
        .bind(row.id)
        .bind(&upper)
        .bind(effective_tier)
        .bind(&request.sector_key)
        .bind(&request.country_key)
        .bind(request.equity_pct)
        .bind(request.bonds_pct)
        .bind(request.cash_pct)
        .bind(now)
        .fetch_one(&mut tx)
        .await?,
        None => db::query_as(&format!(
            "INSERT INTO holding_classification (member_id, ticker, wealth_tier, sector_key, \
             country_key, equity_pct, bonds_pct, cash_pct, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9) RETURNING {COLUMNS}"
        ))
        .bind(auth.member_id)
        .bind(&upper)
        .bind(effective_tier)
        .bind(&request.sector_key)
        .bind(&request.country_key)
        .bind(request.equity_pct)
        .bind(request.bonds_pct)
        .bind(request.cash_pct)
        .bind(now)
        .fetch_one(&mut tx)
        .await?,
    };
    tx.commit().await?;
    Ok(axum::Json(HoldingClassificationResponse {
        ticker: saved.ticker,
        wealth_tier: saved.wealth_tier,
        sector_key: saved.sector_key,
        country_key: saved.country_key,
        equity_pct: saved.equity_pct,
        bonds_pct: saved.bonds_pct,
        cash_pct: saved.cash_pct,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_complex_mix_and_region_codes() {
        let valid = HoldingClassificationRequest {
            wealth_tier: None,
            sector_key: None,
            country_key: Some("XD".into()),
            equity_pct: Some(Decimal::from(75)),
            bonds_pct: Some(Decimal::ZERO),
            cash_pct: Some(Decimal::from(25)),
        };
        let mut errs = Errors::default();
        valid.validate(&mut errs);
        assert!(!errs.has("equityPct") && !errs.has("bondsPct") && !errs.has("cashPct") && !errs.has("countryKey"), "valid mix should pass: {errs:?}");

        let bad_sum = HoldingClassificationRequest {
            wealth_tier: None,
            sector_key: None,
            country_key: Some("XW".into()),
            equity_pct: Some(Decimal::from(75)),
            bonds_pct: Some(Decimal::from(10)),
            cash_pct: Some(Decimal::from(25)),
        };
        let mut errs = Errors::default();
        bad_sum.validate(&mut errs);
        assert!(errs.has("equityPct"), "sum 110 must fail");

        let partial = HoldingClassificationRequest {
            wealth_tier: None,
            sector_key: None,
            country_key: None,
            equity_pct: Some(Decimal::from(75)),
            bonds_pct: None,
            cash_pct: Some(Decimal::from(25)),
        };
        let mut errs = Errors::default();
        partial.validate(&mut errs);
        assert!(errs.has("equityPct"), "partial mix must fail");
    }
}
