//! Data Health Center: runs portfolio integrity, price freshness, FX coverage, classification,
//! cost-basis, and sync-freshness diagnostics for a member, scoring each issue by severity
//! (Critical when >30% of portfolio value is affected) and persisting dismissed issue keys.

use std::collections::{HashMap, HashSet};

use axum::extract::{Path as AxumPath, State};
use chrono::{DateTime, Duration, Utc};
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

const STALE_PRICE_DAYS: i64 = 7;
const STALE_SYNC_DAYS: i64 = 14;
const CRITICAL_PORTFOLIO_SHARE_PCT: f64 = 30.0;
const ERROR_PORTFOLIO_SHARE_PCT: f64 = 10.0;

pub fn register(routes: &mut Routes) {
    routes
        .get("/api/health-center", get_health_report)
        .post("/api/health-center/dismiss", dismiss_issue)
        .delete("/api/health-center/dismiss/{issue_key}", restore_issue);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum HealthSeverity {
    Info,
    Warning,
    Error,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum HealthCategory {
    PriceStaleness,
    FxIntegrity,
    Classification,
    DataConsistency,
    SyncFreshness,
    HistoryGap,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthAffectedItem {
    pub label: String,
    pub detail: Option<String>,
    pub value_eur: f64,
    pub navigate_to: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthIssue {
    pub key: String,
    pub category: HealthCategory,
    pub severity: HealthSeverity,
    pub title_key: String,
    pub description_key: String,
    pub affected_count: usize,
    pub impacted_value_eur: f64,
    pub portfolio_share_pct: f64,
    pub navigate_to: String,
    pub action_label_key: String,
    pub items: Vec<HealthAffectedItem>,
    pub dismissed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthSummary {
    pub critical: usize,
    pub error: usize,
    pub warning: usize,
    pub info: usize,
    pub dismissed: usize,
    pub total_active: usize,
    pub score: u8,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthReport {
    pub status: &'static str,
    pub evaluated_at: DateTime<Utc>,
    pub total_portfolio_eur: f64,
    pub summary: HealthSummary,
    pub issues: Vec<HealthIssue>,
    pub dismissed_issues: Vec<HealthIssue>,
}

pub fn severity_from_share(share_pct: f64, base: HealthSeverity) -> HealthSeverity {
    if share_pct >= CRITICAL_PORTFOLIO_SHARE_PCT {
        HealthSeverity::Critical
    } else if share_pct >= ERROR_PORTFOLIO_SHARE_PCT {
        base.max(HealthSeverity::Error)
    } else {
        base
    }
}

pub fn compute_health_score(issues: &[HealthIssue]) -> u8 {
    let penalty: i32 = issues
        .iter()
        .filter(|i| !i.dismissed)
        .map(|i| match i.severity {
            HealthSeverity::Critical => 30,
            HealthSeverity::Error => 18,
            HealthSeverity::Warning => 8,
            HealthSeverity::Info => 2,
        })
        .sum();
    u8::try_from((100 - penalty).clamp(0, 100)).unwrap_or(0)
}

#[derive(DbFromRow)]
struct AccountRow {
    id: i64,
    name: String,
    account_type: String,
    currency: String,
    current_balance: Decimal,
    is_manual: bool,
    last_synced_at: Option<DateTime<Utc>>,
}

#[derive(DbFromRow)]
struct HoldingRow {
    account_id: i64,
    account_name: String,
    ticker: String,
    name: Option<String>,
    isin: Option<String>,
    quantity: Decimal,
    current_price: Option<Decimal>,
    average_buy_in: Option<Decimal>,
    provider_value_eur: Option<Decimal>,
    provider_pnl_eur: Option<Decimal>,
    last_synced_at: Option<DateTime<Utc>>,
}

#[derive(DbFromRow)]
struct ProfileRow {
    ticker: String,
    isin: Option<String>,
    status: String,
}

#[derive(DbFromRow)]
struct SnapCountRow {
    account_id: i64,
    cnt: i64,
}

fn dec_f64(d: Decimal) -> f64 {
    d.to_f64().unwrap_or(0.0)
}

async fn get_health_report(
    State(state): State<AppState>,
    auth: Auth,
) -> AppResult<axum::Json<HealthReport>> {
    let now = state.clock.now();
    let member_id = auth.member_id;

    let dismissed_keys: HashSet<String> = crate::db::query_scalar::<String>(
        "SELECT issue_key FROM health_dismissal WHERE member_id = $1",
    )
    .bind(member_id)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .collect();

    let accounts: Vec<AccountRow> = crate::db::query_as(
        "SELECT id, name, type::text AS account_type, currency, current_balance, is_manual, last_synced_at \
         FROM account WHERE member_id = $1 AND deleted_at IS NULL AND hidden = false",
    )
    .bind(member_id)
    .fetch_all(&state.db)
    .await?;

    let holdings: Vec<HoldingRow> = crate::db::query_as(
        "SELECT h.account_id, a.name AS account_name, h.ticker, h.name, h.isin, h.quantity, \
                h.current_price, h.average_buy_in, h.provider_value_eur, h.provider_pnl_eur, \
                h.last_synced_at \
         FROM account_holding h \
         JOIN account a ON a.id = h.account_id \
         WHERE a.member_id = $1 AND a.deleted_at IS NULL AND a.hidden = false",
    )
    .bind(member_id)
    .fetch_all(&state.db)
    .await?;

    let profiles: Vec<ProfileRow> = crate::db::query_as(
        "SELECT ticker, isin, COALESCE(status, 'PENDING') AS status FROM security_profile",
    )
    .fetch_all(&state.db)
    .await?;

    let snap_counts: HashMap<i64, i64> = crate::db::query_as::<SnapCountRow>(
        "SELECT bs.account_id, COUNT(*) AS cnt \
         FROM balance_snapshot bs \
         JOIN account a ON a.id = bs.account_id \
         WHERE a.member_id = $1 AND a.deleted_at IS NULL \
         GROUP BY bs.account_id",
    )
    .bind(member_id)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(|r| (r.account_id, r.cnt))
    .collect();

    let total_portfolio_eur: f64 = accounts
        .iter()
        .filter(|a| a.account_type != "LOAN")
        .map(|a| dec_f64(a.current_balance).max(0.0))
        .sum();
    let share_of = |val: f64| -> f64 {
        if total_portfolio_eur > 0.0 {
            ((val / total_portfolio_eur) * 1000.0).round() / 10.0
        } else {
            0.0
        }
    };

    let mut complete_tickers = HashSet::new();
    let mut complete_isins = HashSet::new();
    for p in &profiles {
        if p.status.eq_ignore_ascii_case("COMPLETE") || p.status.eq_ignore_ascii_case("MANUAL") {
            complete_tickers.insert(p.ticker.to_uppercase());
            if let Some(isin) = &p.isin {
                complete_isins.insert(isin.to_uppercase());
            }
        }
    }

    let mut all_issues: Vec<HealthIssue> = Vec::new();
    let stale_price_cutoff = now - Duration::days(STALE_PRICE_DAYS);
    let stale_sync_cutoff = now - Duration::days(STALE_SYNC_DAYS);

    // 1. Price staleness / missing prices
    let mut stale_price_items = Vec::new();
    let mut stale_price_value = 0.0;
    for h in &holdings {
        if h.quantity <= Decimal::ZERO {
            continue;
        }
        let val = h
            .provider_value_eur
            .map(dec_f64)
            .or_else(|| h.current_price.map(|p| dec_f64(p * h.quantity)))
            .unwrap_or(0.0);
        let missing_price = h.current_price.is_none() && h.provider_value_eur.is_none();
        let outdated = h
            .last_synced_at
            .is_some_and(|at| at < stale_price_cutoff);
        if missing_price || outdated {
            stale_price_value += val;
            stale_price_items.push(HealthAffectedItem {
                label: h.name.clone().unwrap_or_else(|| h.ticker.clone()),
                detail: Some(h.account_name.clone()),
                value_eur: val,
                navigate_to: Some(format!("/accounts/{}", h.account_id)),
            });
        }
    }
    if !stale_price_items.is_empty() {
        let share = share_of(stale_price_value);
        all_issues.push(HealthIssue {
            key: "price_staleness".to_owned(),
            category: HealthCategory::PriceStaleness,
            severity: severity_from_share(share, HealthSeverity::Warning),
            title_key: "health.issues.priceStaleness.title".to_owned(),
            description_key: "health.issues.priceStaleness.description".to_owned(),
            affected_count: stale_price_items.len(),
            impacted_value_eur: stale_price_value,
            portfolio_share_pct: share,
            navigate_to: "/sync".to_owned(),
            action_label_key: "health.actions.syncPrices".to_owned(),
            items: stale_price_items.into_iter().take(20).collect(),
            dismissed: dismissed_keys.contains("price_staleness"),
        });
    }

    // 2. Unclassified holdings
    let mut unclassified_items = Vec::new();
    let mut unclassified_value = 0.0;
    for h in &holdings {
        if h.quantity <= Decimal::ZERO {
            continue;
        }
        let is_classified = complete_tickers.contains(&h.ticker.to_uppercase())
            || h.isin
                .as_ref()
                .is_some_and(|i| complete_isins.contains(&i.to_uppercase()));
        if !is_classified {
            let val = h
                .provider_value_eur
                .map(dec_f64)
                .or_else(|| h.current_price.map(|p| dec_f64(p * h.quantity)))
                .unwrap_or(0.0);
            unclassified_value += val;
            unclassified_items.push(HealthAffectedItem {
                label: h.name.clone().unwrap_or_else(|| h.ticker.clone()),
                detail: Some(h.ticker.clone()),
                value_eur: val,
                navigate_to: Some("/analysis".to_owned()),
            });
        }
    }
    if !unclassified_items.is_empty() {
        let share = share_of(unclassified_value);
        all_issues.push(HealthIssue {
            key: "unclassified_holdings".to_owned(),
            category: HealthCategory::Classification,
            severity: severity_from_share(share, HealthSeverity::Warning),
            title_key: "health.issues.unclassified.title".to_owned(),
            description_key: "health.issues.unclassified.description".to_owned(),
            affected_count: unclassified_items.len(),
            impacted_value_eur: unclassified_value,
            portfolio_share_pct: share,
            navigate_to: "/analysis".to_owned(),
            action_label_key: "health.actions.classifyHoldings".to_owned(),
            items: unclassified_items.into_iter().take(20).collect(),
            dismissed: dismissed_keys.contains("unclassified_holdings"),
        });
    }

    // 3. Missing cost basis
    let mut missing_cb_items = Vec::new();
    let mut missing_cb_value = 0.0;
    for h in &holdings {
        if h.quantity <= Decimal::ZERO {
            continue;
        }
        if h.average_buy_in.is_none() && h.provider_pnl_eur.is_none() {
            let val = h
                .provider_value_eur
                .map(dec_f64)
                .or_else(|| h.current_price.map(|p| dec_f64(p * h.quantity)))
                .unwrap_or(0.0);
            missing_cb_value += val;
            missing_cb_items.push(HealthAffectedItem {
                label: h.name.clone().unwrap_or_else(|| h.ticker.clone()),
                detail: Some(h.account_name.clone()),
                value_eur: val,
                navigate_to: Some(format!("/accounts/{}", h.account_id)),
            });
        }
    }
    if !missing_cb_items.is_empty() {
        let share = share_of(missing_cb_value);
        all_issues.push(HealthIssue {
            key: "missing_cost_basis".to_owned(),
            category: HealthCategory::DataConsistency,
            severity: severity_from_share(share, HealthSeverity::Warning),
            title_key: "health.issues.missingCostBasis.title".to_owned(),
            description_key: "health.issues.missingCostBasis.description".to_owned(),
            affected_count: missing_cb_items.len(),
            impacted_value_eur: missing_cb_value,
            portfolio_share_pct: share,
            navigate_to: "/accounts".to_owned(),
            action_label_key: "health.actions.reviewAccounts".to_owned(),
            items: missing_cb_items.into_iter().take(20).collect(),
            dismissed: dismissed_keys.contains("missing_cost_basis"),
        });
    }

    // 4. Non-EUR currency exposure awareness (FX integrity)
    let mut fx_items = Vec::new();
    let mut fx_value = 0.0;
    for a in &accounts {
        if !a.currency.eq_ignore_ascii_case("EUR") && a.current_balance != Decimal::ZERO {
            let val = dec_f64(a.current_balance.abs());
            fx_value += val;
            fx_items.push(HealthAffectedItem {
                label: a.name.clone(),
                detail: Some(a.currency.clone()),
                value_eur: val,
                navigate_to: Some(format!("/accounts/{}", a.id)),
            });
        }
    }
    if !fx_items.is_empty() {
        let share = share_of(fx_value);
        all_issues.push(HealthIssue {
            key: "fx_non_eur_accounts".to_owned(),
            category: HealthCategory::FxIntegrity,
            severity: HealthSeverity::Info,
            title_key: "health.issues.fxAccounts.title".to_owned(),
            description_key: "health.issues.fxAccounts.description".to_owned(),
            affected_count: fx_items.len(),
            impacted_value_eur: fx_value,
            portfolio_share_pct: share,
            navigate_to: "/accounts".to_owned(),
            action_label_key: "health.actions.reviewAccounts".to_owned(),
            items: fx_items.into_iter().take(20).collect(),
            dismissed: dismissed_keys.contains("fx_non_eur_accounts"),
        });
    }

    // 5. Stale connector syncs (>14 days)
    let mut stale_sync_items = Vec::new();
    let mut stale_sync_value = 0.0;
    for a in &accounts {
        if a.is_manual {
            continue;
        }
        if a.last_synced_at.is_none_or(|at| at < stale_sync_cutoff) {
            let val = dec_f64(a.current_balance.abs());
            stale_sync_value += val;
            stale_sync_items.push(HealthAffectedItem {
                label: a.name.clone(),
                detail: a.last_synced_at.map(|d| d.format("%Y-%m-%d").to_string()),
                value_eur: val,
                navigate_to: Some("/sync".to_owned()),
            });
        }
    }
    if !stale_sync_items.is_empty() {
        let share = share_of(stale_sync_value);
        all_issues.push(HealthIssue {
            key: "stale_connector_sync".to_owned(),
            category: HealthCategory::SyncFreshness,
            severity: severity_from_share(share, HealthSeverity::Warning),
            title_key: "health.issues.staleSync.title".to_owned(),
            description_key: "health.issues.staleSync.description".to_owned(),
            affected_count: stale_sync_items.len(),
            impacted_value_eur: stale_sync_value,
            portfolio_share_pct: share,
            navigate_to: "/sync".to_owned(),
            action_label_key: "health.actions.openSync".to_owned(),
            items: stale_sync_items.into_iter().take(20).collect(),
            dismissed: dismissed_keys.contains("stale_connector_sync"),
        });
    }

    // 6. Accounts missing historical snapshots
    let mut no_snap_items = Vec::new();
    let mut no_snap_value = 0.0;
    for a in &accounts {
        if a.current_balance != Decimal::ZERO && snap_counts.get(&a.id).copied().unwrap_or(0) == 0 {
            let val = dec_f64(a.current_balance.abs());
            no_snap_value += val;
            no_snap_items.push(HealthAffectedItem {
                label: a.name.clone(),
                detail: Some(a.account_type.clone()),
                value_eur: val,
                navigate_to: Some(format!("/accounts/{}", a.id)),
            });
        }
    }
    if !no_snap_items.is_empty() {
        let share = share_of(no_snap_value);
        all_issues.push(HealthIssue {
            key: "missing_history_snapshots".to_owned(),
            category: HealthCategory::HistoryGap,
            severity: HealthSeverity::Info,
            title_key: "health.issues.historyGap.title".to_owned(),
            description_key: "health.issues.historyGap.description".to_owned(),
            affected_count: no_snap_items.len(),
            impacted_value_eur: no_snap_value,
            portfolio_share_pct: share,
            navigate_to: "/accounts".to_owned(),
            action_label_key: "health.actions.reviewAccounts".to_owned(),
            items: no_snap_items.into_iter().take(20).collect(),
            dismissed: dismissed_keys.contains("missing_history_snapshots"),
        });
    }

    all_issues.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| b.impacted_value_eur.total_cmp(&a.impacted_value_eur))
    });

    let (dismissed_issues, issues): (Vec<_>, Vec<_>) =
        all_issues.into_iter().partition(|i| i.dismissed);

    let critical = issues.iter().filter(|i| i.severity == HealthSeverity::Critical).count();
    let error = issues.iter().filter(|i| i.severity == HealthSeverity::Error).count();
    let warning = issues.iter().filter(|i| i.severity == HealthSeverity::Warning).count();
    let info = issues.iter().filter(|i| i.severity == HealthSeverity::Info).count();
    let score = compute_health_score(&issues);
    let status = if critical > 0 {
        "CRITICAL"
    } else if error > 0 {
        "DEGRADED"
    } else if warning > 0 {
        "ATTENTION"
    } else {
        "HEALTHY"
    };

    Ok(axum::Json(HealthReport {
        status,
        evaluated_at: now,
        total_portfolio_eur,
        summary: HealthSummary {
            critical,
            error,
            warning,
            info,
            dismissed: dismissed_issues.len(),
            total_active: issues.len(),
            score,
        },
        issues,
        dismissed_issues,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DismissRequest {
    issue_key: String,
}

impl Validate for DismissRequest {
    fn validate(&self, errors: &mut Errors) {
        let k = self.issue_key.trim();
        if k.is_empty() || k.len() > 160 {
            errors.add("issueKey", "issueKey must be between 1 and 160 characters");
        }
    }
}

async fn dismiss_issue(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<DismissRequest>,
) -> AppResult<StatusCode> {
    let key = req.issue_key.trim();
    crate::db::query(
        "INSERT INTO health_dismissal (member_id, issue_key, dismissed_at) \
         VALUES ($1, $2, $3) \
         ON CONFLICT (member_id, issue_key) DO UPDATE SET dismissed_at = EXCLUDED.dismissed_at",
    )
    .bind(auth.member_id)
    .bind(key)
    .bind(state.clock.now())
    .execute(&state.db)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn restore_issue(
    State(state): State<AppState>,
    auth: Auth,
    AxumPath(issue_key): AxumPath<String>,
) -> AppResult<StatusCode> {
    let trimmed = issue_key.trim();
    if trimmed.is_empty() {
        return Err(AppError::BadRequest("issueKey is required".to_owned()));
    }
    crate::db::query(
        "DELETE FROM health_dismissal WHERE member_id = $1 AND issue_key = $2",
    )
    .bind(auth.member_id)
    .bind(trimmed)
    .execute(&state.db)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_escalates_above_10_and_30_percent_of_portfolio() {
        assert_eq!(severity_from_share(5.0, HealthSeverity::Warning), HealthSeverity::Warning);
        assert_eq!(severity_from_share(12.5, HealthSeverity::Warning), HealthSeverity::Error);
        assert_eq!(severity_from_share(31.0, HealthSeverity::Warning), HealthSeverity::Critical);
    }

    #[test]
    fn score_clamps_and_ignores_dismissed_issues() {
        let mk = |severity, dismissed| HealthIssue {
            key: "k".into(),
            category: HealthCategory::PriceStaleness,
            severity,
            title_key: String::new(),
            description_key: String::new(),
            affected_count: 1,
            impacted_value_eur: 100.0,
            portfolio_share_pct: 10.0,
            navigate_to: "/sync".into(),
            action_label_key: String::new(),
            items: vec![],
            dismissed,
        };
        assert_eq!(compute_health_score(&[]), 100);
        assert_eq!(
            compute_health_score(&[
                mk(HealthSeverity::Critical, false),
                mk(HealthSeverity::Error, true),
                mk(HealthSeverity::Warning, false),
            ]),
            62
        );
    }
}
