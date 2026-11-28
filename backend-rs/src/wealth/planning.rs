//! Retirement / FIRE simulator and Save-Up goal calculator.

use axum::extract::State;
use chrono::Datelike;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::auth::Auth;
use crate::error::AppResult;
use crate::http::json::{Errors, Json, Validate};
use crate::routes::Routes;

pub fn register(routes: &mut Routes) {
    routes
        .post("/api/wealth/planning/retirement", simulate_retirement)
        .post("/api/wealth/planning/save-up", simulate_save_up);
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetirementRequest {
    pub current_age: u32,
    pub retirement_age: u32,
    pub life_expectancy: u32,
    pub current_savings_eur: f64,
    pub monthly_contribution_eur: f64,
    pub expected_annual_return_pct: f64,
    pub inflation_pct: f64,
    pub monthly_retirement_spend_eur: f64,
    pub safe_withdrawal_rate_pct: f64,
}

impl Validate for RetirementRequest {
    fn validate(&self, errors: &mut Errors) {
        if self.current_age < 15 || self.current_age > 100 {
            errors.add("currentAge", "currentAge must be between 15 and 100");
        }
        if self.retirement_age < self.current_age || self.retirement_age > 105 {
            errors.add("retirementAge", "retirementAge must be >= currentAge");
        }
        if self.life_expectancy <= self.retirement_age || self.life_expectancy > 120 {
            errors.add("lifeExpectancy", "lifeExpectancy must be > retirementAge");
        }
        if self.safe_withdrawal_rate_pct <= 0.0 || self.safe_withdrawal_rate_pct > 15.0 {
            errors.add("safeWithdrawalRatePct", "safeWithdrawalRatePct must be in (0, 15]");
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetirementYearPoint {
    pub age: u32,
    pub year: i32,
    pub phase: &'static str,
    pub balance_nominal_eur: f64,
    pub balance_real_eur: f64,
    pub annual_contribution_eur: f64,
    pub annual_withdrawal_eur: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetirementSimulationResponse {
    pub fi_number_eur: f64,
    pub coast_fire_number_eur: f64,
    pub projected_at_retirement_eur: f64,
    pub projected_real_at_retirement_eur: f64,
    pub years_to_fi: Option<u32>,
    pub fi_age: Option<u32>,
    pub sustainable_monthly_withdrawal_eur: f64,
    pub depletion_age: Option<u32>,
    pub funded_ratio_pct: f64,
    pub points: Vec<RetirementYearPoint>,
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

pub fn run_retirement_simulation(
    req: &RetirementRequest,
    start_year: i32,
) -> RetirementSimulationResponse {
    let annual_spend_real = req.monthly_retirement_spend_eur * 12.0;
    let swr = (req.safe_withdrawal_rate_pct / 100.0).max(0.005);
    let fi_number = annual_spend_real / swr;

    let nominal_r = req.expected_annual_return_pct / 100.0;
    let inflation = req.inflation_pct / 100.0;
    let real_r = ((1.0 + nominal_r) / (1.0 + inflation)) - 1.0;

    let years_to_ret = req.retirement_age.saturating_sub(req.current_age);
    let coast_fire = if years_to_ret > 0 && real_r > -0.9 {
        fi_number / (1.0 + real_r).powi(years_to_ret as i32)
    } else {
        fi_number
    };

    let mut balance_nominal = req.current_savings_eur.max(0.0);
    let mut fi_age: Option<u32> = None;
    let mut depletion_age: Option<u32> = None;
    let mut projected_at_ret = balance_nominal;
    let mut projected_real_at_ret = balance_nominal;
    let mut points = Vec::new();

    for offset in 0..=(req.life_expectancy.saturating_sub(req.current_age)) {
        let age = req.current_age + offset;
        let year = start_year + offset as i32;
        let inflation_factor = (1.0 + inflation).powi(offset as i32);
        let balance_real = if inflation_factor > 0.0 {
            balance_nominal / inflation_factor
        } else {
            balance_nominal
        };

        if fi_age.is_none() && balance_real >= fi_number {
            fi_age = Some(age);
        }
        if age == req.retirement_age {
            projected_at_ret = balance_nominal;
            projected_real_at_ret = balance_real;
        }

        let in_retirement = age >= req.retirement_age;
        let annual_contrib = if in_retirement {
            0.0
        } else {
            req.monthly_contribution_eur.max(0.0) * 12.0 * inflation_factor
        };
        let annual_withdraw = if in_retirement {
            annual_spend_real * inflation_factor
        } else {
            0.0
        };

        points.push(RetirementYearPoint {
            age,
            year,
            phase: if in_retirement { "RETIREMENT" } else { "ACCUMULATION" },
            balance_nominal_eur: round2(balance_nominal.max(0.0)),
            balance_real_eur: round2(balance_real.max(0.0)),
            annual_contribution_eur: round2(annual_contrib),
            annual_withdrawal_eur: round2(annual_withdraw),
        });

        let next_balance = balance_nominal * (1.0 + nominal_r) + annual_contrib - annual_withdraw;
        if next_balance <= 0.0 {
            if in_retirement && depletion_age.is_none() {
                depletion_age = Some(age + 1);
            }
            balance_nominal = 0.0;
        } else {
            balance_nominal = next_balance;
        }
    }

    let sustainable_monthly = (projected_real_at_ret * swr) / 12.0;
    let funded_ratio = if fi_number > 0.0 {
        ((req.current_savings_eur / fi_number) * 1000.0).round() / 10.0
    } else {
        100.0
    };

    RetirementSimulationResponse {
        fi_number_eur: round2(fi_number),
        coast_fire_number_eur: round2(coast_fire),
        projected_at_retirement_eur: round2(projected_at_ret),
        projected_real_at_retirement_eur: round2(projected_real_at_ret),
        years_to_fi: fi_age.map(|a| a.saturating_sub(req.current_age)),
        fi_age,
        sustainable_monthly_withdrawal_eur: round2(sustainable_monthly),
        depletion_age,
        funded_ratio_pct: funded_ratio,
        points,
    }
}

async fn simulate_retirement(
    State(state): State<AppState>,
    _auth: Auth,
    Json(req): Json<RetirementRequest>,
) -> AppResult<axum::Json<RetirementSimulationResponse>> {
    Ok(axum::Json(run_retirement_simulation(
        &req,
        state.clock.today().year(),
    )))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveUpRequest {
    pub target_amount_eur: f64,
    pub current_savings_eur: f64,
    pub monthly_contribution_eur: f64,
    pub expected_annual_return_pct: f64,
}

impl Validate for SaveUpRequest {
    fn validate(&self, errors: &mut Errors) {
        if self.target_amount_eur <= 0.0 {
            errors.add("targetAmountEur", "targetAmountEur must be > 0");
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveUpResponse {
    pub months_required: Option<u32>,
    pub years_required: Option<f64>,
    pub total_contributed_eur: f64,
    pub total_growth_eur: f64,
    pub final_balance_eur: f64,
}

pub fn run_save_up_simulation(req: &SaveUpRequest) -> SaveUpResponse {
    let mut balance = req.current_savings_eur.max(0.0);
    let mut contributed = balance;
    if balance >= req.target_amount_eur {
        return SaveUpResponse {
            months_required: Some(0),
            years_required: Some(0.0),
            total_contributed_eur: round2(contributed),
            total_growth_eur: 0.0,
            final_balance_eur: round2(balance),
        };
    }

    let monthly_rate = (1.0 + req.expected_annual_return_pct / 100.0).powf(1.0 / 12.0) - 1.0;
    let monthly = req.monthly_contribution_eur.max(0.0);

    for month in 1..=1200u32 {
        balance = balance * (1.0 + monthly_rate) + monthly;
        contributed += monthly;
        if balance >= req.target_amount_eur {
            let growth = (balance - contributed).max(0.0);
            return SaveUpResponse {
                months_required: Some(month),
                years_required: Some(((month as f64 / 12.0) * 10.0).round() / 10.0),
                total_contributed_eur: round2(contributed),
                total_growth_eur: round2(growth),
                final_balance_eur: round2(balance),
            };
        }
    }

    SaveUpResponse {
        months_required: None,
        years_required: None,
        total_contributed_eur: round2(contributed),
        total_growth_eur: round2((balance - contributed).max(0.0)),
        final_balance_eur: round2(balance),
    }
}

async fn simulate_save_up(
    _auth: Auth,
    Json(req): Json<SaveUpRequest>,
) -> AppResult<axum::Json<SaveUpResponse>> {
    Ok(axum::Json(run_save_up_simulation(&req)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fire_simulator_computes_fi_number_coast_fire_and_longevity() {
        let req = RetirementRequest {
            current_age: 35,
            retirement_age: 60,
            life_expectancy: 90,
            current_savings_eur: 100_000.0,
            monthly_contribution_eur: 1_500.0,
            expected_annual_return_pct: 7.0,
            inflation_pct: 2.0,
            monthly_retirement_spend_eur: 2_500.0,
            safe_withdrawal_rate_pct: 4.0,
        };
        let res = run_retirement_simulation(&req, 2026);
        assert_eq!(res.fi_number_eur, 750_000.0);
        assert!(res.coast_fire_number_eur < res.fi_number_eur);
        assert!(res.fi_age.is_some());
        assert!(res.depletion_age.is_none());
    }

    #[test]
    fn save_up_reaches_target_faster_with_compound_return() {
        let zero = run_save_up_simulation(&SaveUpRequest {
            target_amount_eur: 22_000.0,
            current_savings_eur: 10_000.0,
            monthly_contribution_eur: 500.0,
            expected_annual_return_pct: 0.0,
        });
        assert_eq!(zero.months_required, Some(24));

        let with_return = run_save_up_simulation(&SaveUpRequest {
            target_amount_eur: 22_000.0,
            current_savings_eur: 10_000.0,
            monthly_contribution_eur: 500.0,
            expected_annual_return_pct: 8.0,
        });
        assert!(with_return.months_required.unwrap() < 24);
        assert!(with_return.total_growth_eur > 0.0);
    }
}
