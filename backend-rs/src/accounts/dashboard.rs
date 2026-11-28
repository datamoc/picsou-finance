//! DashboardService: totals, net-worth history over the range, distributions, liabilities and
//! goal summaries of the member's visible accounts.

use std::collections::HashMap;

use axum::extract::{RawQuery, State};
use chrono::{Datelike, NaiveDate};
use rust_decimal::{Decimal, RoundingStrategy};
use serde::Serialize;

use super::history::{NetWorthPoint, build_history_preloaded};
use super::jdecimal;
use super::query::Params;
use super::scale::{add, sub};
use crate::AppState;
use crate::auth::Auth;
use crate::error::AppResult;
use crate::ledger::decimal::divide;
use crate::ledger::model::{Account, AccountType};
use crate::ledger::{access, loans, valuation};

const HUNDRED: Decimal = Decimal::from_parts(100, 0, 0, false, 0);

#[serde_with::skip_serializing_none]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardResponse {
    #[serde(with = "jdecimal")]
    total_net_worth: Decimal,
    #[serde(with = "jdecimal")]
    total_liabilities: Decimal,
    #[serde(with = "jdecimal::option")]
    total_monthly_payment: Option<Decimal>,
    net_worth_history: Vec<NetWorthPoint>,
    distribution: Vec<DistributionItem>,
    assets: Vec<DistributionItem>,
    liabilities: Vec<LiabilityEntry>,
    goal_summaries: Vec<serde_json::Value>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DistributionItem {
    account_id: i64,
    name: String,
    color: String,
    #[serde(with = "jdecimal")]
    balance_eur: Decimal,
    /// A Java double, rounded to two places with Math.round.
    #[serde(with = "jdecimal::double")]
    percentage: Decimal,
    account_type: AccountType,
    has_holdings: bool,
    /// Remaining balance (EUR, member-weighted) of visible loans linked to this asset.
    #[serde(with = "jdecimal::option")]
    linked_debt_eur: Option<Decimal>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiabilityEntry {
    account_id: i64,
    name: String,
    color: String,
    #[serde(with = "jdecimal")]
    balance_eur: Decimal,
    #[serde(with = "jdecimal::double")]
    percentage: Decimal,
    account_type: AccountType,
    has_holdings: bool,
    #[serde(with = "jdecimal::option")]
    monthly_payment: Option<Decimal>,
    #[serde(with = "jdecimal::double::option")]
    percent_paid: Option<Decimal>,
    #[serde(with = "jdecimal::option")]
    payment_due_amount_eur: Option<Decimal>,
    payment_due_date: Option<NaiveDate>,
}

/// The months of history a dashboard range shows; the range never filters the totals.
pub fn range_months(range: Option<&str>, today: NaiveDate) -> i32 {
    match range.unwrap_or("1Y") {
        "7D" | "1M" => 1,
        "3M" => 3,
        "YTD" => today.month() as i32,
        "ALL" => 1200,
        _ => 12,
    }
}

/// Math.round(x * 100) / 100.0: half rounds toward positive infinity.
fn round_cents(x: Decimal) -> Decimal {
    ((x * HUNDRED) + Decimal::new(5, 1)).floor() / HUNDRED
}

fn percentage(balance: Decimal, divisor: Decimal) -> Decimal {
    if divisor > Decimal::ZERO {
        round_cents(divide(balance, divisor, 6, RoundingStrategy::MidpointAwayFromZero) * HUNDRED)
    } else {
        Decimal::ZERO
    }
}

fn distribution(
    accounts: &[&Account],
    divisor: Decimal,
    holding_counts: &HashMap<i64, usize>,
    values: &HashMap<i64, Decimal>,
    linked_debts: &HashMap<i64, Decimal>,
    liabilities_only: bool,
) -> Vec<DistributionItem> {
    accounts
        .iter()
        .filter(|a| {
            a.parent_account_id.is_none() && liabilities_only == a.account_type.is_liability()
        })
        .map(|a| {
            let balance = values.get(&a.id).copied().unwrap_or(Decimal::ZERO);
            let linked_debt_eur = (!liabilities_only)
                .then(|| linked_debts.get(&a.id).copied())
                .flatten()
                .filter(|d| *d > Decimal::ZERO);
            DistributionItem {
                account_id: a.id,
                name: a.name.clone(),
                color: a.color.clone(),
                balance_eur: balance,
                percentage: percentage(balance, divisor),
                account_type: a.account_type,
                has_holdings: holding_counts.get(&a.id).copied().unwrap_or(0) > 0,
                linked_debt_eur,
            }
        })
        .collect()
}

pub async fn get_dashboard(
    State(state): State<AppState>,
    auth: Auth,
    RawQuery(query): RawQuery,
) -> AppResult<axum::Json<DashboardResponse>> {
    let range = Params::parse(query.as_deref()).string("range");
    let member_id = auth.member_id;
    let today = state.clock.today();
    let prices = &*state.ledger.prices;
    let mut conn = state.db.pool_handle();
    let mut visible = access::readable_accounts(&mut conn, member_id).await?;
    super::scale::fix_accounts(&mut visible);
    visible.retain(|a| !a.hidden);
    let shares = access::shares_for(&mut conn, &visible, member_id).await?;
    let batch = valuation::ValuationBatch::load(&mut conn, &visible).await?;
    let holding_counts: HashMap<i64, usize> = visible
        .iter()
        .map(|a| (a.id, batch.holdings.get(&a.id).map_or(0, Vec::len)))
        .collect();

    let mut total_assets = Decimal::ZERO;
    let mut total_liabilities = Decimal::ZERO;
    let mut values: HashMap<i64, Decimal> = HashMap::new();
    let mut unweighted_liabilities: HashMap<i64, Decimal> = HashMap::new();
    // Child accounts (pockets) are already inside their parent's balance.
    let top_level_accounts: Vec<Account> = visible
        .iter()
        .filter(|a| a.parent_account_id.is_none())
        .cloned()
        .collect();
    let mut valuations: HashMap<i64, valuation::Valuation> =
        HashMap::with_capacity(top_level_accounts.len());
    for a in &top_level_accounts {
        let val = valuation::valuation_from_batch(prices, a, &batch, today).await;
        valuations.insert(a.id, val);
        let value = match a.account_type {
            AccountType::Loan => val.live_eur,
            AccountType::CreditCard => {
                -prices
                    .to_eur(
                        Some(a.current_balance),
                        &a.currency,
                        a.ticker.as_deref(),
                        today,
                    )
                    .await
            }
            _ if holding_counts[&a.id] == 0 => {
                prices
                    .to_eur(
                        Some(a.current_balance),
                        &a.currency,
                        a.ticker.as_deref(),
                        today,
                    )
                    .await
            }
            _ => val.live_eur,
        };
        if a.account_type.is_liability() {
            unweighted_liabilities.insert(a.id, value);
        }
        let share = shares.get(&a.id).copied().unwrap_or(Decimal::ZERO);
        let value = access::weigh(value, share);
        values.insert(a.id, value);
        if a.account_type.is_liability() {
            total_liabilities = add(total_liabilities, value);
        } else {
            total_assets = add(total_assets, value);
        }
    }
    let total_net_worth = sub(total_assets, total_liabilities);

    let months = range_months(range.as_deref(), today);
    let history = build_history_preloaded(
        &state,
        &mut conn,
        &top_level_accounts,
        &shares,
        months,
        false,
        &batch,
        &valuations,
    )
    .await?;

    let debts = &batch.debts;
    // Sum visible, member-weighted loan balances by the asset account each loan finances.
    let mut linked_debt_by_asset: HashMap<i64, Decimal> = HashMap::new();
    for a in &top_level_accounts {
        if a.account_type.is_liability()
            && let Some(linked_id) = debts.get(&a.id).and_then(|d| d.linked_account_id)
        {
            let owed = values.get(&a.id).copied().unwrap_or(Decimal::ZERO).abs();
            if owed > Decimal::ZERO {
                let entry = linked_debt_by_asset.entry(linked_id).or_insert(Decimal::ZERO);
                *entry = add(*entry, owed);
            }
        }
    }

    let all: Vec<&Account> = visible.iter().collect();
    let distributable: Vec<&Account> = visible
        .iter()
        .filter(|a| !a.account_type.is_liability() && a.account_type != AccountType::PeerLoan)
        .collect();
    let distributable_total: Decimal = distributable
        .iter()
        .map(|a| values.get(&a.id).copied().unwrap_or(Decimal::ZERO))
        .fold(Decimal::ZERO, add);
    let distribution_items = distribution(
        &distributable,
        distributable_total,
        &holding_counts,
        &values,
        &linked_debt_by_asset,
        false,
    );
    let assets = distribution(
        &all,
        total_assets,
        &holding_counts,
        &values,
        &linked_debt_by_asset,
        false,
    );
    let raw_liabilities = distribution(
        &all,
        total_liabilities,
        &holding_counts,
        &values,
        &linked_debt_by_asset,
        true,
    );

    let by_id: HashMap<i64, &Account> = visible.iter().map(|a| (a.id, a)).collect();
    let mut total_monthly: Option<Decimal> = None;
    let mut entries = Vec::with_capacity(raw_liabilities.len());
    for item in raw_liabilities {
        let a = by_id[&item.account_id];
        let share = shares.get(&a.id).copied().unwrap_or(Decimal::ZERO);
        let (mut due_amount, mut due_date) = (None, None);
        if a.account_type == AccountType::CreditCard {
            if let Some(amount) = a.payment_due_amount {
                due_amount = Some(access::weigh(
                    prices
                        .to_eur(Some(amount), &a.currency, a.ticker.as_deref(), today)
                        .await,
                    share,
                ));
            }
            due_date = a.payment_due_date;
        }
        let (mut monthly, mut percent_paid) = (None, None);
        if let Some(debt) = debts.get(&a.id) {
            monthly = loans::resolve_monthly_payment(debt).map(|m| access::weigh(m, share));
            if debt.borrowed_amount > Decimal::ZERO {
                let remaining = unweighted_liabilities[&a.id].abs();
                let repaid = sub(debt.borrowed_amount, remaining);
                let paid = divide(
                    repaid,
                    debt.borrowed_amount,
                    6,
                    RoundingStrategy::MidpointAwayFromZero,
                ) * HUNDRED;
                percent_paid = Some(paid.clamp(Decimal::ZERO, HUNDRED));
            }
            if let Some(m) = monthly {
                total_monthly = Some(total_monthly.unwrap_or(Decimal::ZERO) + m);
            }
        }
        entries.push(LiabilityEntry {
            account_id: item.account_id,
            name: item.name,
            color: item.color,
            balance_eur: item.balance_eur,
            percentage: item.percentage,
            account_type: item.account_type,
            has_holdings: item.has_holdings,
            monthly_payment: monthly,
            percent_paid,
            payment_due_amount_eur: due_amount,
            payment_due_date: due_date,
        });
    }
    let goal_summaries = state.accounts.goals.summaries(&state, member_id).await?;
    Ok(axum::Json(DashboardResponse {
        total_net_worth,
        total_liabilities,
        total_monthly_payment: total_monthly,
        net_worth_history: history,
        distribution: distribution_items,
        assets,
        liabilities: entries,
        goal_summaries,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_account(id: i64, name: &str, account_type: AccountType) -> Account {
        Account {
            id,
            member_id: 1,
            name: name.to_owned(),
            account_type,
            provider: None,
            currency: "EUR".to_owned(),
            current_balance: Decimal::ZERO,
            cash_balance: None,
            last_synced_at: None,
            payment_due_amount: None,
            payment_due_date: None,
            card_nature: None,
            reward_points: None,
            external_account_id: None,
            is_manual: true,
            color: "#3b82f6".to_owned(),
            iban: None,
            ticker: None,
            logo_url: None,
            logo_key: None,
            institution_name: None,
            institution_domain: None,
            requisition_id: None,
            opened_at: None,
            provider_invested_eur: None,
            provider_paid_in_eur: None,
            scheduled_payment_eur: None,
            scheduled_payment_cadence: None,
            deleted_at: None,
            parent_account_id: None,
            hidden: false,
            tracked_isin: None,
            created_at: chrono::DateTime::UNIX_EPOCH,
            updated_at: chrono::DateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn distribution_attaches_linked_debt_only_to_financed_assets() {
        let property = test_account(1, "Appartement", AccountType::RealEstate);
        let pea = test_account(2, "PEA", AccountType::Pea);
        let accounts = vec![&property, &pea];
        let holding_counts = HashMap::from([(1, 0), (2, 3)]);
        let values = HashMap::from([(1, Decimal::new(900_000, 0)), (2, Decimal::new(100_000, 0))]);
        let linked_debts = HashMap::from([(1, Decimal::new(512_683_03, 2))]);

        let items = distribution(
            &accounts,
            Decimal::new(1_000_000, 0),
            &holding_counts,
            &values,
            &linked_debts,
            false,
        );
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].linked_debt_eur, Some(Decimal::new(512_683_03, 2)));
        assert_eq!(items[1].linked_debt_eur, None);

        let json_prop = serde_json::to_value(&items[0]).unwrap();
        let json_pea = serde_json::to_value(&items[1]).unwrap();
        assert_eq!(json_prop["linkedDebtEur"], serde_json::json!(512_683.03));
        assert!(json_pea.get("linkedDebtEur").is_none());
    }
}
