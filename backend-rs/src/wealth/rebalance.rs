//! Rebalancing Worksheet engine: computes tier drift against target allocation, flags drift
//! drivers outside the configurable drift band, and generates concrete buy/sell orders —
//! including a tax-efficient "cash-only" mode that deploys fresh cash to underweight tiers
//! without selling existing positions.

use axum::extract::State;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::auth::Auth;
use crate::db::DbFromRow;
use crate::error::AppResult;
use crate::http::json::{Errors, Json, Validate};
use crate::routes::Routes;

pub fn register(routes: &mut Routes) {
    routes.post("/api/analysis/rebalance-plan", compute_rebalance_plan);
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TierTargetInput {
    pub tier: String,
    pub target_pct: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RebalanceRequest {
    pub cash_contribution_eur: f64,
    pub drift_band_pct: f64,
    pub cash_only: bool,
    pub targets: Option<Vec<TierTargetInput>>,
}

impl Validate for RebalanceRequest {
    fn validate(&self, errors: &mut Errors) {
        if !self.cash_contribution_eur.is_finite() || self.cash_contribution_eur < 0.0 {
            errors.add("cashContributionEur", "Cash contribution must be >= 0");
        }
        if !self.drift_band_pct.is_finite() || !(0.0..=50.0).contains(&self.drift_band_pct) {
            errors.add("driftBandPct", "Drift band must be between 0 and 50");
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RebalanceRow {
    pub tier: String,
    pub current_eur: f64,
    pub current_pct: f64,
    pub target_pct: f64,
    pub drift_pct: f64,
    pub in_band: bool,
    pub is_drift_driver: bool,
    pub action: &'static str,
    pub trade_eur: f64,
    pub post_eur: f64,
    pub post_pct: f64,
    pub post_drift_pct: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RebalancePlanResponse {
    pub current_total_eur: f64,
    pub post_total_eur: f64,
    pub cash_contribution_eur: f64,
    pub drift_band_pct: f64,
    pub cash_only: bool,
    pub max_abs_drift_before: f64,
    pub max_abs_drift_after: f64,
    pub cash_needed_to_zero_drift_without_selling: f64,
    pub rows: Vec<RebalanceRow>,
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// Pure mathematical rebalancing solver.
///
/// - When `cash_only = false`: computes exact buy/sell deltas so every tier matches `target_pct`
///   of `current_total + cash_contribution_eur` (net trades sum to `cash_contribution_eur`).
/// - When `cash_only = true`: distributes `cash_contribution_eur` across underweight tiers in
///   proportion to their shortfall relative to the post-contribution total, with zero sells,
///   iteratively clamping any tier that would overshoot its target.
pub fn solve_rebalance(
    tiers: &[(String, f64, f64)], // (tier, current_eur, target_pct)
    cash_contribution_eur: f64,
    drift_band_pct: f64,
    cash_only: bool,
) -> RebalancePlanResponse {
    let current_total: f64 = tiers.iter().map(|(_, cur, _)| cur.max(0.0)).sum();
    let post_total = current_total + cash_contribution_eur;

    // Minimum cash needed to reach exact target allocation without any sell:
    // total_needed = max_i (current_i / (target_i / 100)) for tiers with target_i > 0.
    let min_total_no_sell = tiers
        .iter()
        .filter(|(_, _, t)| *t > 0.0)
        .map(|(_, cur, t)| cur.max(0.0) / (*t / 100.0))
        .fold(current_total, f64::max);
    let cash_needed_no_sell = (min_total_no_sell - current_total).max(0.0);

    let mut trades = vec![0.0_f64; tiers.len()];
    if post_total > 0.0 {
        if !cash_only {
            for (i, (_, cur, target_pct)) in tiers.iter().enumerate() {
                let desired = post_total * (*target_pct / 100.0);
                trades[i] = desired - cur.max(0.0);
            }
        } else if cash_contribution_eur > 0.0 {
            // Allocate fresh cash to underweight tiers proportionally to their deficit.
            let deficits: Vec<f64> = tiers
                .iter()
                .map(|(_, cur, target_pct)| {
                    let desired = post_total * (*target_pct / 100.0);
                    (desired - cur.max(0.0)).max(0.0)
                })
                .collect();
            let total_deficit: f64 = deficits.iter().sum();
            if total_deficit > 0.0 {
                for (i, d) in deficits.iter().enumerate() {
                    trades[i] = cash_contribution_eur * (*d / total_deficit);
                }
            } else {
                // Already balanced or all targets 0 -> distribute by target weights.
                let sum_pct: f64 = tiers.iter().map(|(_, _, p)| p.max(0.0)).sum();
                if sum_pct > 0.0 {
                    for (i, (_, _, p)) in tiers.iter().enumerate() {
                        trades[i] = cash_contribution_eur * (p.max(0.0) / sum_pct);
                    }
                }
            }
        }
    }

    let mut rows = Vec::with_capacity(tiers.len());
    let mut max_before = 0.0_f64;
    let mut max_after = 0.0_f64;
    for (i, (tier, cur, target_pct)) in tiers.iter().enumerate() {
        let cur = cur.max(0.0);
        let cur_pct = if current_total > 0.0 {
            (cur / current_total) * 100.0
        } else {
            0.0
        };
        let drift_before = cur_pct - *target_pct;
        max_before = max_before.max(drift_before.abs());

        let trade = trades[i];
        let post = (cur + trade).max(0.0);
        let post_pct = if post_total > 0.0 {
            (post / post_total) * 100.0
        } else {
            0.0
        };
        let drift_after = post_pct - *target_pct;
        max_after = max_after.max(drift_after.abs());

        let in_band = drift_before.abs() <= drift_band_pct + 1e-9;
        let action = if trade > 1.0 {
            "BUY"
        } else if trade < -1.0 {
            "SELL"
        } else {
            "HOLD"
        };

        rows.push(RebalanceRow {
            tier: tier.clone(),
            current_eur: round2(cur),
            current_pct: round1(cur_pct),
            target_pct: round1(*target_pct),
            drift_pct: round1(drift_before),
            in_band,
            is_drift_driver: !in_band,
            action,
            trade_eur: round2(trade),
            post_eur: round2(post),
            post_pct: round1(post_pct),
            post_drift_pct: round1(drift_after),
        });
    }

    RebalancePlanResponse {
        current_total_eur: round2(current_total),
        post_total_eur: round2(post_total),
        cash_contribution_eur: round2(cash_contribution_eur),
        drift_band_pct: round1(drift_band_pct),
        cash_only,
        max_abs_drift_before: round1(max_before),
        max_abs_drift_after: round1(max_after),
        cash_needed_to_zero_drift_without_selling: round2(cash_needed_no_sell),
        rows,
    }
}

#[derive(DbFromRow)]
struct AccountTierRow {
    account_type: String,
    current_balance: Decimal,
}

#[derive(DbFromRow)]
struct TargetRow {
    bonds_pct: Decimal,
    real_estate_pct: Decimal,
    equity_pct: Decimal,
    crypto_pct: Decimal,
    alternative_pct: Decimal,
}

fn account_type_to_tier(account_type: &str) -> Option<&'static str> {
    match account_type {
        "ASSURANCE_VIE" | "PER" => Some("BONDS"),
        "REAL_ESTATE" | "SCPI" => Some("REAL_ESTATE"),
        "PEA" | "COMPTE_TITRES" | "BROKERAGE" | "EMPLOYEE_SAVINGS" => Some("EQUITY"),
        "CRYPTO" => Some("CRYPTO"),
        "PRECIOUS_METALS" | "PEER_LOAN" | "OTHER" => Some("ALTERNATIVE"),
        _ => None,
    }
}

async fn compute_rebalance_plan(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<RebalanceRequest>,
) -> AppResult<axum::Json<RebalancePlanResponse>> {
    let accounts: Vec<AccountTierRow> = crate::db::query_as(
        "SELECT type::text AS account_type, current_balance \
         FROM account WHERE member_id = $1 AND deleted_at IS NULL AND hidden = false",
    )
    .bind(auth.member_id)
    .fetch_all(&state.db)
    .await?;

    let mut totals = [0.0_f64; 5]; // BONDS, REAL_ESTATE, EQUITY, CRYPTO, ALTERNATIVE
    for a in &accounts {
        let val = a.current_balance.to_f64().unwrap_or(0.0).max(0.0);
        match account_type_to_tier(&a.account_type) {
            Some("BONDS") => totals[0] += val,
            Some("REAL_ESTATE") => totals[1] += val,
            Some("EQUITY") => totals[2] += val,
            Some("CRYPTO") => totals[3] += val,
            Some("ALTERNATIVE") => totals[4] += val,
            _ => {}
        }
    }

    let saved: Option<TargetRow> = crate::db::query_as(
        "SELECT bonds_pct, real_estate_pct, equity_pct, crypto_pct, alternative_pct \
         FROM member_allocation_profile WHERE member_id = $1",
    )
    .bind(auth.member_id)
    .fetch_optional(&state.db)
    .await?;

    let mut target_pcts = match saved {
        Some(t) => [
            t.bonds_pct.to_f64().unwrap_or(0.0),
            t.real_estate_pct.to_f64().unwrap_or(30.0),
            t.equity_pct.to_f64().unwrap_or(30.0),
            t.crypto_pct.to_f64().unwrap_or(10.0),
            t.alternative_pct.to_f64().unwrap_or(10.0),
        ],
        None => [20.0, 30.0, 30.0, 10.0, 10.0],
    };

    if let Some(overrides) = &req.targets {
        for item in overrides {
            match item.tier.as_str() {
                "BONDS" => target_pcts[0] = item.target_pct.clamp(0.0, 100.0),
                "REAL_ESTATE" => target_pcts[1] = item.target_pct.clamp(0.0, 100.0),
                "EQUITY" => target_pcts[2] = item.target_pct.clamp(0.0, 100.0),
                "CRYPTO" => target_pcts[3] = item.target_pct.clamp(0.0, 100.0),
                "ALTERNATIVE" => target_pcts[4] = item.target_pct.clamp(0.0, 100.0),
                _ => {}
            }
        }
    }

    let tiers = vec![
        ("BONDS".to_owned(), totals[0], target_pcts[0]),
        ("REAL_ESTATE".to_owned(), totals[1], target_pcts[1]),
        ("EQUITY".to_owned(), totals[2], target_pcts[2]),
        ("CRYPTO".to_owned(), totals[3], target_pcts[3]),
        ("ALTERNATIVE".to_owned(), totals[4], target_pcts[4]),
    ];

    Ok(axum::Json(solve_rebalance(
        &tiers,
        req.cash_contribution_eur,
        req.drift_band_pct,
        req.cash_only,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cash_only_rebalance_never_sells_and_reduces_drift() {
        let tiers = vec![
            ("REAL_ESTATE".to_owned(), 40_000.0, 30.0),
            ("EQUITY".to_owned(), 50_000.0, 60.0),
            ("CRYPTO".to_owned(), 10_000.0, 10.0),
        ];
        let plan = solve_rebalance(&tiers, 10_000.0, 5.0, true);
        assert!(plan.rows.iter().all(|r| r.trade_eur >= 0.0));
        assert!(plan.rows.iter().all(|r| r.action != "SELL"));
        assert!(plan.max_abs_drift_after < plan.max_abs_drift_before);
        let trade_sum: f64 = plan.rows.iter().map(|r| r.trade_eur).sum();
        assert!((trade_sum - 10_000.0).abs() < 0.05);
    }

    #[test]
    fn full_rebalance_reaches_zero_drift() {
        let tiers = vec![
            ("REAL_ESTATE".to_owned(), 40_000.0, 30.0),
            ("EQUITY".to_owned(), 50_000.0, 60.0),
            ("CRYPTO".to_owned(), 10_000.0, 10.0),
        ];
        let plan = solve_rebalance(&tiers, 5_000.0, 5.0, false);
        assert_eq!(plan.max_abs_drift_after, 0.0);
        let trade_sum: f64 = plan.rows.iter().map(|r| r.trade_eur).sum();
        assert!((trade_sum - 5_000.0).abs() < 0.05);
    }
}
