//! Lucya (app.lucya.com), life insurance and PER: one call signs in, reads the contracts and
//! writes one account per contract. Nothing about the sign-in is stored, neither a session nor the
//! password, so there is no scheduled sync: refreshing means signing in again.

use std::collections::HashSet;

use axum::Json as AxumJson;
use axum::extract::State;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::sidecar::SyncError;
use super::sidecar::portfolio::{self, PortfolioAccount};
use super::sidecar::web::consume;
use crate::AppState;
use crate::auth::Auth;
use crate::db::AsDbHandle;
use crate::error::{AppError, AppResult};
use crate::http::client_ip::ClientIp;
use crate::http::json::{Errors, Json, Validate};
use crate::http::rate_limit::policies;
use crate::ledger::accounts;
use crate::ledger::members;
use crate::ledger::model::AccountType;
use crate::native_sidecars::lucya::{self, Contract};
use crate::routes::Routes;

const PROVIDER: &str = "Lucya";
const EXTERNAL_ID_PREFIX: &str = "lucya_";
const ACCOUNT_COLOR: &str = "#0f766e";
/// The width of `account.name`.
const ACCOUNT_NAME_MAX: usize = 100;
const AUTH_RATE_LIMITED: &str = "Too many Lucya sign-in attempts. Please wait before trying again.";

pub fn register(routes: &mut Routes) {
    routes.post("/api/lucya/refresh", refresh);
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RefreshRequest {
    #[serde(default)]
    identifier: String,
    #[serde(default)]
    password: String,
}

/// The reader itself refuses an empty or oversized identifier or password (INVALID_DATA).
impl Validate for RefreshRequest {
    fn validate(&self, _errors: &mut Errors) {}
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RefreshResponse {
    contract_count: usize,
}

async fn refresh(
    State(state): State<AppState>,
    auth: Auth,
    ip: ClientIp,
    Json(req): Json<RefreshRequest>,
) -> AppResult<AxumJson<RefreshResponse>> {
    consume(&state, &policies::LUCYA_AUTH, &ip, AUTH_RATE_LIMITED)?;
    let base = lucya::base_url(state.config.native_sidecar_url("lucya"));
    let contracts = lucya::read_contracts(base, &req.identifier, &req.password)
        .await
        .map_err(|refused| {
            AppError::from(SyncError::with_code(
                refused.detail,
                "Could not read the Lucya contracts",
            ))
        })?;
    import(&state, auth.member_id, &contracts).await?;
    Ok(AxumJson(RefreshResponse {
        contract_count: contracts.len(),
    }))
}

/// A contract as it is about to be written.
struct Imported {
    external_id: String,
    name: String,
    account_type: AccountType,
    value_eur: Decimal,
}

/// The envelope the page's wording names. An unknown one stops the whole import rather than being
/// filed under a guess.
fn account_type(kind: &str) -> Option<AccountType> {
    let kind = kind.to_lowercase();
    if kind.contains("assurance vie") || kind.contains("assurance-vie") {
        return Some(AccountType::AssuranceVie);
    }
    let retirement = kind.contains("épargne retraite")
        || kind
            .split(|c: char| !c.is_alphanumeric())
            .any(|word| word == "per" || word == "perin");
    retirement.then_some(AccountType::Per)
}

fn prepare(contract: &Contract) -> AppResult<Imported> {
    let account_type = account_type(&contract.kind).ok_or_else(|| {
        tracing::warn!("Lucya listed a contract of an unknown type");
        SyncError::with_code("UPSTREAM_FORMAT_CHANGED", "Unknown Lucya contract type")
    })?;
    Ok(Imported {
        external_id: format!("{EXTERNAL_ID_PREFIX}{}", contract.id),
        name: contract.name.chars().take(ACCOUNT_NAME_MAX).collect(),
        account_type,
        value_eur: contract.value_eur,
    })
}

/// Writes the contracts in one transaction. Signing in is a deliberate act, so a listed contract
/// the member had deleted comes back; an imported one the page no longer lists closes at zero.
async fn import(state: &AppState, member_id: i64, contracts: &[Contract]) -> AppResult<()> {
    let contracts = contracts
        .iter()
        .map(prepare)
        .collect::<AppResult<Vec<_>>>()?;
    let listed: HashSet<&str> = contracts.iter().map(|c| c.external_id.as_str()).collect();
    let synced_at = state.clock.now();

    let mut tx = state.db.begin().await?;
    // Two refreshes of one member never interleave, so a contract is created once.
    members::lock_or_throw(&mut tx, member_id).await?;
    accounts::restore_soft_deleted_by_provider_excluding(
        &mut tx.as_handle(),
        member_id,
        PROVIDER,
        |id| !id.is_some_and(|id| listed.contains(id)),
    )
    .await?;
    let imported = accounts::find_all_by_member_and_provider_for_update(
        &mut tx.as_handle(),
        member_id,
        PROVIDER,
    )
    .await?;
    for contract in &contracts {
        let data = PortfolioAccount {
            external_id: &contract.external_id,
            name: &contract.name,
            account_type: contract.account_type,
            provider: PROVIDER,
            color: ACCOUNT_COLOR,
            balance_eur: contract.value_eur,
            cash_balance: None,
            // The page reports no cost basis: the value stands in, so no gain is invented.
            invested_eur: contract.value_eur,
        };
        portfolio::write(state, &mut tx.as_handle(), member_id, &data, &[], synced_at).await?;
    }
    for mut account in imported {
        let gone = account
            .external_account_id
            .as_deref()
            .is_some_and(|id| id.starts_with(EXTERNAL_ID_PREFIX) && !listed.contains(id));
        if !gone || account.current_balance.is_zero() {
            continue;
        }
        tracing::info!(
            "Lucya no longer lists an imported contract, closing it at zero (member={member_id}; account={})",
            account.id
        );
        account.current_balance = Decimal::ZERO;
        account.last_synced_at = Some(synced_at);
        let saved = accounts::update(&mut tx.as_handle(), &account, synced_at).await?;
        portfolio::replace_holdings(
            state,
            &mut tx.as_handle(),
            saved.id,
            &[],
            Decimal::ZERO,
            Decimal::ZERO,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_known_wording_names_an_envelope() {
        for (kind, expected) in [
            ("Assurance vie", Some(AccountType::AssuranceVie)),
            ("ASSURANCE-VIE", Some(AccountType::AssuranceVie)),
            ("PER", Some(AccountType::Per)),
            ("PER individuel", Some(AccountType::Per)),
            ("Plan d'Épargne Retraite", Some(AccountType::Per)),
            ("Contrat de capitalisation", None),
            ("Persistant", None),
        ] {
            assert_eq!(account_type(kind), expected, "{kind}");
        }
    }
}
