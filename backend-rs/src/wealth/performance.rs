//! Portfolio Performance Engine: Time-Weighted Return (TWR), Money-Weighted Return (MWR / XIRR
//! with Modified Dietz fallback), volatility, max drawdown, snapshot-vs-transaction tracking mode
//! separation, and normalized Base-100 benchmark comparison.

use std::collections::{BTreeMap, HashMap};

use axum::extract::{Query, State};
use chrono::{Duration, NaiveDate};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::auth::Auth;
use crate::db::DbFromRow;
use crate::error::AppResult;
use crate::routes::Routes;

pub fn register(routes: &mut Routes) {
    routes
        .get("/api/wealth/performance", get_performance)
        .get("/api/wealth/benchmarks", list_benchmarks);
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkOption {
    pub id: &'static str,
    pub ticker: &'static str,
    pub label: &'static str,
}

pub const BENCHMARKS: &[BenchmarkOption] = &[
    BenchmarkOption {
        id: "MSCI_WORLD",
        ticker: "CW8.PA",
        label: "MSCI World (CW8)",
    },
    BenchmarkOption {
        id: "SP500",
        ticker: "^GSPC",
        label: "S&P 500",
    },
    BenchmarkOption {
        id: "CAC40",
        ticker: "^FCHI",
        label: "CAC 40",
    },
    BenchmarkOption {
        id: "EUROSTOXX50",
        ticker: "^STOXX50E",
        label: "Euro Stoxx 50",
    },
    BenchmarkOption {
        id: "NASDAQ100",
        ticker: "^NDX",
        label: "Nasdaq 100",
    },
];

async fn list_benchmarks(_auth: Auth) -> axum::Json<&'static [BenchmarkOption]> {
    axum::Json(BENCHMARKS)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PerformanceQuery {
    range: Option<String>,
    portfolio_id: Option<i64>,
    benchmark: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerformancePoint {
    pub date: NaiveDate,
    pub value_eur: f64,
    pub twr_index: f64,
    pub twr_pct: f64,
    pub benchmark_index: Option<f64>,
    pub benchmark_pct: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerformanceSummary {
    pub range: String,
    pub from_date: Option<NaiveDate>,
    pub to_date: Option<NaiveDate>,
    pub start_value_eur: f64,
    pub end_value_eur: f64,
    pub net_contributions_eur: f64,
    pub total_gain_eur: f64,
    pub twr_pct: f64,
    pub annualized_twr_pct: Option<f64>,
    pub mwr_pct: f64,
    pub max_drawdown_pct: f64,
    pub volatility_pct: Option<f64>,
    pub transaction_mode_accounts: usize,
    pub snapshot_mode_accounts: usize,
    pub benchmark_id: Option<String>,
    pub benchmark_label: Option<String>,
    pub benchmark_return_pct: Option<f64>,
    pub series: Vec<PerformancePoint>,
}

#[derive(Debug, Clone)]
pub struct ValuationObservation {
    pub date: NaiveDate,
    pub value: f64,
    pub external_cashflow: f64,
}

/// Computes daily-linked TWR, Modified Dietz / XIRR MWR, max drawdown, and annualized volatility
/// from ordered valuation observations with external cashflows.
pub fn compute_performance_math(
    observations: &[ValuationObservation],
    benchmark_prices: &BTreeMap<NaiveDate, f64>,
) -> (f64, Option<f64>, f64, f64, Option<f64>, Vec<PerformancePoint>) {
    if observations.is_empty() {
        return (0.0, None, 0.0, 0.0, None, Vec::new());
    }

    let first_bench = observations.iter().find_map(|o| {
        benchmark_prices
            .range(..=o.date)
            .next_back()
            .map(|(_, v)| *v)
            .filter(|v| *v > 0.0)
    });

    let mut cumulative_factor = 1.0_f64;
    let mut peak_index = 100.0_f64;
    let mut max_dd = 0.0_f64;
    let mut daily_returns = Vec::new();
    let mut series = Vec::with_capacity(observations.len());

    for (idx, obs) in observations.iter().enumerate() {
        if idx > 0 {
            let prev = &observations[idx - 1];
            // Modified Dietz single-period return neutralizes external cashflows.
            let denom = prev.value + 0.5 * obs.external_cashflow;
            let r = if denom.abs() > 1e-6 {
                ((obs.value - prev.value - obs.external_cashflow) / denom).clamp(-0.95, 5.0)
            } else {
                0.0
            };
            cumulative_factor *= 1.0 + r;
            daily_returns.push(r);
        }
        let twr_index = cumulative_factor * 100.0;
        if twr_index > peak_index {
            peak_index = twr_index;
        }
        if peak_index > 0.0 {
            let dd = ((peak_index - twr_index) / peak_index) * 100.0;
            if dd > max_dd {
                max_dd = dd;
            }
        }

        let (bench_idx, bench_pct) = match first_bench {
            Some(base) if base > 0.0 => {
                if let Some((_, price)) = benchmark_prices.range(..=obs.date).next_back() {
                    let bi = (*price / base) * 100.0;
                    (Some((bi * 100.0).round() / 100.0), Some(((bi - 100.0) * 100.0).round() / 100.0))
                } else {
                    (None, None)
                }
            }
            _ => (None, None),
        };

        series.push(PerformancePoint {
            date: obs.date,
            value_eur: (obs.value * 100.0).round() / 100.0,
            twr_index: (twr_index * 100.0).round() / 100.0,
            twr_pct: ((twr_index - 100.0) * 100.0).round() / 100.0,
            benchmark_index: bench_idx,
            benchmark_pct: bench_pct,
        });
    }

    let twr_pct = ((cumulative_factor - 1.0) * 10000.0).round() / 100.0;

    let first = &observations[0];
    let last = &observations[observations.len() - 1];
    let span_days = (last.date - first.date).num_days().max(0) as f64;

    let annualized_twr = if span_days >= 30.0 && cumulative_factor > 0.0 {
        let ann = (cumulative_factor.powf(365.25 / span_days) - 1.0) * 100.0;
        Some((ann * 100.0).round() / 100.0)
    } else {
        None
    };

    // Money-Weighted Return: XIRR via Newton-Raphson when span >= 30d, else Modified Dietz.
    let mwr_pct = compute_xirr_or_dietz(observations, span_days);

    // Annualized volatility of sub-period returns.
    let volatility_pct = if daily_returns.len() >= 5 && span_days > 0.0 {
        let n = daily_returns.len() as f64;
        let mean = daily_returns.iter().sum::<f64>() / n;
        let variance = daily_returns
            .iter()
            .map(|r| (r - mean).powi(2))
            .sum::<f64>()
            / (n - 1.0);
        let periods_per_year = (n / span_days) * 365.25;
        let ann_vol = variance.sqrt() * periods_per_year.sqrt() * 100.0;
        if ann_vol.is_finite() {
            Some((ann_vol * 100.0).round() / 100.0)
        } else {
            None
        }
    } else {
        None
    };

    (
        twr_pct,
        annualized_twr,
        (mwr_pct * 100.0).round() / 100.0,
        (max_dd * 100.0).round() / 100.0,
        volatility_pct,
        series,
    )
}

fn compute_xirr_or_dietz(observations: &[ValuationObservation], span_days: f64) -> f64 {
    if observations.len() < 2 {
        return 0.0;
    }
    let first = &observations[0];
    let last = &observations[observations.len() - 1];

    // Modified Dietz weighted cashflows (always well-defined)
    let mut weighted_cf = 0.0_f64;
    let mut total_cf = 0.0_f64;
    for obs in observations.iter().skip(1) {
        let elapsed = (obs.date - first.date).num_days().max(0) as f64;
        let weight = if span_days > 0.0 {
            1.0 - (elapsed / span_days)
        } else {
            0.5
        };
        total_cf += obs.external_cashflow;
        weighted_cf += weight * obs.external_cashflow;
    }
    let dietz_denom = first.value + weighted_cf;
    let dietz = if dietz_denom.abs() > 1e-6 {
        ((last.value - first.value - total_cf) / dietz_denom) * 100.0
    } else {
        0.0
    };

    if span_days < 30.0 {
        return dietz;
    }

    // Attempt XIRR (annualized IRR) via Newton-Raphson over dated cashflows
    let mut flows: Vec<(f64, f64)> = Vec::new();
    if first.value.abs() > 1e-6 {
        flows.push((0.0, -first.value));
    }
    for obs in observations.iter().skip(1) {
        if obs.external_cashflow.abs() > 1e-6 {
            let t = (obs.date - first.date).num_days() as f64 / 365.25;
            flows.push((t, -obs.external_cashflow));
        }
    }
    flows.push((span_days / 365.25, last.value));

    let mut rate = (dietz / 100.0).clamp(-0.8, 3.0);
    for _ in 0..40 {
        let mut f = 0.0;
        let mut df = 0.0;
        let base = 1.0 + rate;
        if base <= 1e-4 {
            return dietz;
        }
        for &(t, cf) in &flows {
            let pow = base.powf(-t);
            f += cf * pow;
            df -= t * cf * pow / base;
        }
        if f.abs() < 1e-7 {
            return rate * 100.0;
        }
        if df.abs() < 1e-10 {
            break;
        }
        let next = rate - f / df;
        if !next.is_finite() || next <= -0.99 || next > 20.0 {
            return dietz;
        }
        if (next - rate).abs() < 1e-7 {
            return next * 100.0;
        }
        rate = next;
    }
    dietz
}

#[derive(DbFromRow)]
struct PerfAccountRow {
    id: i64,
    account_type: String,
}

#[derive(DbFromRow)]
struct SnapshotRow {
    account_id: i64,
    date: NaiveDate,
    balance: Decimal,
}

#[derive(DbFromRow)]
struct FlowRow {
    date: NaiveDate,
    net_flow: Decimal,
}

#[derive(DbFromRow)]
struct BenchPriceRow {
    date: NaiveDate,
    price_eur: Decimal,
}

fn resolve_from_date(today: NaiveDate, range: &str) -> NaiveDate {
    match range {
        "1M" => today - Duration::days(30),
        "3M" => today - Duration::days(90),
        "YTD" => NaiveDate::from_ymd_opt(today.year(), 1, 1).unwrap_or(today - Duration::days(365)),
        "1Y" => today - Duration::days(365),
        "3Y" => today - Duration::days(365 * 3),
        "5Y" => today - Duration::days(365 * 5),
        "ALL" => today - Duration::days(365 * 25),
        _ => today - Duration::days(365),
    }
}

use chrono::Datelike;

async fn get_performance(
    State(state): State<AppState>,
    auth: Auth,
    Query(q): Query<PerformanceQuery>,
) -> AppResult<axum::Json<PerformanceSummary>> {
    let range = q.range.as_deref().unwrap_or("1Y").to_uppercase();
    let today = state.clock.today();
    let from = resolve_from_date(today, &range);

    let accounts: Vec<PerfAccountRow> = if let Some(pid) = q.portfolio_id {
        crate::db::query_as(
            "SELECT a.id, a.type::text AS account_type \
             FROM account a \
             JOIN virtual_portfolio_account vpa ON vpa.account_id = a.id \
             JOIN virtual_portfolio vp ON vp.id = vpa.portfolio_id \
             WHERE vp.id = $1 AND vp.member_id = $2 AND a.deleted_at IS NULL AND a.hidden = false",
        )
        .bind(pid)
        .bind(auth.member_id)
        .fetch_all(&state.db)
        .await?
    } else {
        crate::db::query_as(
            "SELECT id, type::text AS account_type \
             FROM account WHERE member_id = $1 AND deleted_at IS NULL AND hidden = false \
               AND type::text <> 'LOAN'",
        )
        .bind(auth.member_id)
        .fetch_all(&state.db)
        .await?
    };

    let account_ids: Vec<i64> = accounts.iter().map(|a| a.id).collect();
    let mut tx_mode = 0usize;
    let mut snap_mode = 0usize;
    for a in &accounts {
        if matches!(
            a.account_type.as_str(),
            "ASSURANCE_VIE" | "PER" | "SCPI" | "REAL_ESTATE" | "PRECIOUS_METALS"
        ) {
            snap_mode += 1;
        } else {
            tx_mode += 1;
        }
    }

    if account_ids.is_empty() {
        return Ok(axum::Json(PerformanceSummary {
            range,
            from_date: None,
            to_date: None,
            start_value_eur: 0.0,
            end_value_eur: 0.0,
            net_contributions_eur: 0.0,
            total_gain_eur: 0.0,
            twr_pct: 0.0,
            annualized_twr_pct: None,
            mwr_pct: 0.0,
            max_drawdown_pct: 0.0,
            volatility_pct: None,
            transaction_mode_accounts: 0,
            snapshot_mode_accounts: 0,
            benchmark_id: None,
            benchmark_label: None,
            benchmark_return_pct: None,
            series: Vec::new(),
        }));
    }

    let snaps: Vec<SnapshotRow> = crate::db::query_as(
        "SELECT account_id, date, balance \
         FROM balance_snapshot \
         WHERE account_id = ANY($1) AND date >= $2 \
         ORDER BY date ASC",
    )
    .bind(&account_ids)
    .bind(from)
    .fetch_all(&state.db)
    .await?;

    let flows: HashMap<NaiveDate, f64> = crate::db::query_as::<FlowRow>(
        "SELECT date, COALESCE(SUM(CASE \
                WHEN tx_type = 'DEPOSIT' THEN ABS(amount) \
                WHEN tx_type = 'WITHDRAWAL' THEN -ABS(amount) \
                ELSE 0 END), 0) AS net_flow \
         FROM transaction \
         WHERE account_id = ANY($1) AND date >= $2 AND tx_type IN ('DEPOSIT', 'WITHDRAWAL') \
         GROUP BY date",
    )
    .bind(&account_ids)
    .bind(from)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(|r| (r.date, r.net_flow.to_f64().unwrap_or(0.0)))
    .collect();

    // Forward-fill per account across all observed snapshot dates
    let mut per_account: HashMap<i64, BTreeMap<NaiveDate, f64>> = HashMap::new();
    let mut dates = std::collections::BTreeSet::new();
    for s in snaps {
        let val = s.balance.to_f64().unwrap_or(0.0);
        per_account.entry(s.account_id).or_default().insert(s.date, val);
        dates.insert(s.date);
    }

    let mut observations = Vec::with_capacity(dates.len());
    let mut prev_date: Option<NaiveDate> = None;
    for d in dates {
        let total: f64 = account_ids
            .iter()
            .filter_map(|id| {
                per_account
                    .get(id)
                    .and_then(|m| m.range(..=d).next_back().map(|(_, v)| *v))
            })
            .sum();
        let cf: f64 = flows
            .iter()
            .filter(|(fd, _)| prev_date.is_some_and(|p| **fd > p && **fd <= d))
            .map(|(_, v)| *v)
            .sum();
        observations.push(ValuationObservation {
            date: d,
            value: total,
            external_cashflow: cf,
        });
        prev_date = Some(d);
    }

    // Optional benchmark series from price_snapshot
    let bench_opt = q.benchmark.as_deref().and_then(|id| {
        BENCHMARKS
            .iter()
            .find(|b| b.id.eq_ignore_ascii_case(id) || b.ticker.eq_ignore_ascii_case(id))
    });
    let mut bench_prices = BTreeMap::new();
    if let Some(bench) = bench_opt {
        let rows: Vec<BenchPriceRow> = crate::db::query_as(
            "SELECT date, price_eur FROM price_snapshot \
             WHERE UPPER(ticker) = UPPER($1) AND date >= $2 ORDER BY date ASC",
        )
        .bind(bench.ticker)
        .bind(from)
        .fetch_all(&state.db)
        .await?;
        for r in rows {
            if let Some(v) = r.price_eur.to_f64().filter(|v| *v > 0.0) {
                bench_prices.insert(r.date, v);
            }
        }
    }

    let (twr_pct, annualized_twr_pct, mwr_pct, max_drawdown_pct, volatility_pct, series) =
        compute_performance_math(&observations, &bench_prices);

    let start_value = observations.first().map_or(0.0, |o| o.value);
    let end_value = observations.last().map_or(0.0, |o| o.value);
    let net_contributions: f64 = observations.iter().skip(1).map(|o| o.external_cashflow).sum();
    let total_gain = end_value - start_value - net_contributions;
    let bench_return = series.last().and_then(|p| p.benchmark_pct);

    Ok(axum::Json(PerformanceSummary {
        range,
        from_date: observations.first().map(|o| o.date),
        to_date: observations.last().map(|o| o.date),
        start_value_eur: (start_value * 100.0).round() / 100.0,
        end_value_eur: (end_value * 100.0).round() / 100.0,
        net_contributions_eur: (net_contributions * 100.0).round() / 100.0,
        total_gain_eur: (total_gain * 100.0).round() / 100.0,
        twr_pct,
        annualized_twr_pct,
        mwr_pct,
        max_drawdown_pct,
        volatility_pct,
        transaction_mode_accounts: tx_mode,
        snapshot_mode_accounts: snap_mode,
        benchmark_id: bench_opt.map(|b| b.id.to_owned()),
        benchmark_label: bench_opt.map(|b| b.label.to_owned()),
        benchmark_return_pct: bench_return,
        series,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twr_is_invariant_to_external_deposits() {
        let d = |m, day| NaiveDate::from_ymd_opt(2026, m, day).unwrap();
        // Start with 10_000, deposit 10_000 on day 2 with 0% market move -> TWR must stay 0%, not +100%.
        let obs = vec![
            ValuationObservation { date: d(1, 1), value: 10_000.0, external_cashflow: 0.0 },
            ValuationObservation { date: d(2, 1), value: 20_000.0, external_cashflow: 10_000.0 },
            ValuationObservation { date: d(3, 1), value: 22_000.0, external_cashflow: 0.0 },
        ];
        let (twr, _, mwr, max_dd, _, series) = compute_performance_math(&obs, &BTreeMap::new());
        assert!((twr - 10.0).abs() < 0.05, "expected ~10% TWR, got {twr}");
        assert!(mwr > 0.0);
        assert_eq!(max_dd, 0.0);
        assert_eq!(series.len(), 3);
    }

    #[test]
    fn max_drawdown_tracks_peak_to_trough() {
        let d = |day| NaiveDate::from_ymd_opt(2026, 1, day).unwrap();
        let obs = vec![
            ValuationObservation { date: d(1), value: 100.0, external_cashflow: 0.0 },
            ValuationObservation { date: d(2), value: 120.0, external_cashflow: 0.0 },
            ValuationObservation { date: d(3), value: 90.0, external_cashflow: 0.0 },
            ValuationObservation { date: d(4), value: 110.0, external_cashflow: 0.0 },
        ];
        let (_, _, _, max_dd, _, _) = compute_performance_math(&obs, &BTreeMap::new());
        // Peak 120 -> trough 90 = 25% drawdown
        assert!((max_dd - 25.0).abs() < 0.05, "expected 25% drawdown, got {max_dd}");
    }
}
