//! LinxeaSyncService and LinxeaAdapter: the sidecar calls, the strict validation of one contract
//! snapshot (all or nothing), and its write over the member's accounts.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::Duration;

use crate::db::{AsDbHandle, DbHandle};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use regex::Regex;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::AppState;
use crate::connectors::sidecar::portfolio::{self, PortfolioAccount};
use crate::connectors::sidecar::template::error;
use crate::connectors::sidecar::{
    AuthInitResponse, JobResult, SessionStatusResponse, SidecarClient, SidecarHttp,
    SidecarInitiate, SidecarSession, SidecarSessionState, SyncError, SyncJob, SyncStrategy,
    SyncTemplate,
};
use crate::error::{AppError, AppResult};
use crate::ledger::members;
use crate::ledger::model::{AccountType, HoldingDraft};
use crate::ledger::providers::LINXEA as PROVIDER;
use crate::ledger::sidecar::{SidecarBank, SidecarErrorCode, SourceSyncResult};
use crate::ledger::sync_values::{clean, limit, money_close, sum_complete, weighted_average};

const BANK: SidecarBank = SidecarBank::Linxea;
const AUTH_TIMEOUT: Duration = Duration::from_secs(60);
const ACCOUNTS_TIMEOUT: Duration = Duration::from_secs(180);
const WEBVIEW_TIMEOUT: Duration = Duration::from_secs(30);
const WEBVIEW_ATTEMPT_SECONDS: i64 = 360;

pub fn webview_supported(state: &AppState) -> bool {
    state.config.desktop.eq_ignore_ascii_case("true")
}

#[derive(Debug, Clone)]
struct PendingWebview {
    member_id: i64,
    deadline: DateTime<Utc>,
    generation: u64,
}

#[derive(Default)]
struct WebviewMaps {
    pending: HashMap<String, PendingWebview>,
    generations: HashMap<i64, u64>,
}

#[derive(Default)]
struct WebviewLogins {
    maps: Mutex<WebviewMaps>,
    write: tokio::sync::Mutex<()>,
}

impl WebviewLogins {
    fn maps(&self) -> MutexGuard<'_, WebviewMaps> {
        self.maps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn generation(&self, member_id: i64) -> u64 {
        self.maps()
            .generations
            .get(&member_id)
            .copied()
            .unwrap_or(0)
    }

    fn evict_expired(&self, now: DateTime<Utc>) {
        self.maps().pending.retain(|_, p| p.deadline > now);
    }

    fn remove(&self, process_id: &str) {
        self.maps().pending.remove(process_id);
    }

    fn owned_attempt(
        &self,
        process_id: &str,
        member_id: i64,
        now: DateTime<Utc>,
    ) -> Result<PendingWebview, SyncError> {
        self.maps()
            .pending
            .get(process_id)
            .filter(|p| p.member_id == member_id && p.deadline > now)
            .cloned()
            .ok_or_else(|| {
                error(
                    SidecarErrorCode::AuthAttemptExpired,
                    "Linxea sign-in attempt expired",
                )
            })
    }

    fn require_generation(&self, member_id: i64, generation: u64) -> Result<(), SyncError> {
        if self.generation(member_id) != generation {
            return Err(error(
                SidecarErrorCode::AuthAttemptExpired,
                "The Linxea connection was removed during the login",
            ));
        }
        Ok(())
    }

    fn forget_member(&self, member_id: i64) -> Vec<String> {
        let mut maps = self.maps();
        *maps.generations.entry(member_id).or_insert(0) += 1;
        let webviews: Vec<String> = maps
            .pending
            .iter()
            .filter(|(_, p)| p.member_id == member_id)
            .map(|(id, _)| id.clone())
            .collect();
        maps.pending.retain(|_, p| p.member_id != member_id);
        webviews
    }
}

fn webview_logins(state: &AppState) -> Arc<WebviewLogins> {
    state.connectors.jobs.local::<WebviewLogins>("linxea-webview")
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebviewStartResponse {
    pub process_id: String,
    pub status: String,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebviewStatusResponse {
    pub status: String,
    pub error_code: Option<SidecarErrorCode>,
    pub session: Option<SessionStatusResponse>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebviewCancelResponse {
    pub cancelled: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct BridgeWebviewStart {
    process_id: Option<String>,
    status: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct BridgeWebviewStatus {
    status: Option<String>,
    session_state: Option<String>,
    error_code: Option<String>,
}

fn require_webview(state: &AppState) -> AppResult<()> {
    if webview_supported(state) {
        Ok(())
    } else {
        Err(AppError::NotFound(
            "Linxea interactive WebView login is only available in Picsou Desktop".to_owned(),
        ))
    }
}

static ISIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("^[A-Z]{2}[A-Z0-9]{10}$").expect("regex"));
static CURRENCY: LazyLock<Regex> = LazyLock::new(|| Regex::new("^[A-Z]{3}$").expect("regex"));

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Position {
    isin: Option<String>,
    symbol: Option<String>,
    label: Option<String>,
    quantity: Option<Decimal>,
    buying_price_eur: Option<Decimal>,
    current_price: Option<Decimal>,
    quote_currency: Option<String>,
    current_value_eur: Option<Decimal>,
    pnl_eur: Option<Decimal>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountData {
    external_id: Option<String>,
    name: Option<String>,
    #[serde(rename = "type")]
    account_type: Option<AccountType>,
    balance_eur: Option<Decimal>,
    cash_balance: Option<Decimal>,
    positions: Option<Vec<Option<Position>>>,
    #[serde(default)]
    snapshot_complete: bool,
}

pub struct PreparedAccount {
    external_id: String,
    name: String,
    account_type: AccountType,
    balance_eur: Decimal,
    cash_balance: Decimal,
    invested_amount_eur: Decimal,
    positions: Vec<PreparedPosition>,
}

#[derive(Debug, Clone)]
struct PreparedPosition {
    ticker: String,
    isin: Option<String>,
    name: String,
    quantity: Decimal,
    average_buy_in_eur: Option<Decimal>,
    current_price: Option<Decimal>,
    quote_currency: Option<String>,
    current_value_eur: Decimal,
    pnl_eur: Option<Decimal>,
}

fn client(state: &AppState) -> SidecarClient {
    SidecarClient::new(state, BANK, SidecarErrorCode::AuthAttemptExpired)
}

fn template() -> SyncTemplate<Strategy> {
    SyncTemplate::new(BANK, Strategy, false)
}

pub struct Strategy;

#[async_trait]
impl SyncStrategy for Strategy {
    type Prepared = Vec<PreparedAccount>;

    async fn fetch(&self, state: &AppState, job: &SyncJob) -> JobResult<Vec<PreparedAccount>> {
        let fetched: Vec<Option<AccountData>> = client(state)
            .post_for_list(
                "/accounts",
                &json!({ "sessionState": job.plain_state }),
                ACCOUNTS_TIMEOUT,
                "Could not fetch Linxea contracts",
                SidecarErrorCode::PortfolioIncomplete,
                "Linxea returned no complete contract",
            )
            .await?;
        Ok(prepare_accounts(state, fetched).await?)
    }

    async fn commit(
        &self,
        state: &AppState,
        conn: &mut DbHandle<'_>,
        _session: &mut SidecarSession,
        job: &SyncJob,
        prepared: &Vec<PreparedAccount>,
        synced_at: DateTime<Utc>,
    ) -> JobResult<()> {
        if members::find_by_id(&mut *conn, job.member_id)
            .await?
            .is_none()
        {
            return Err(AppError::NotFound("Family member not found".to_owned()).into());
        }
        for data in prepared {
            upsert_account(state, conn, data, job.member_id, synced_at).await?;
        }
        Ok(())
    }

    fn describe(&self, prepared: &Vec<PreparedAccount>) -> String {
        format!("accounts={}", prepared.len())
    }
}

pub async fn initiate_auth(
    state: &AppState,
    login: &str,
    password: &str,
    member_id: i64,
) -> AppResult<AuthInitResponse> {
    let result: SidecarInitiate = client(state)
        .post(
            "/initiate",
            &json!({ "login": login, "password": password }),
            AUTH_TIMEOUT,
            "Could not initiate Linxea authentication",
            Some(SidecarErrorCode::InvalidCredentials),
        )
        .await?;
    if !result.mfa_required {
        let session_state = result
            .session_state
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                error(
                    SidecarErrorCode::InvalidData,
                    "Linxea did not return a session",
                )
            })?;
        store_session_and_queue(state, session_state, member_id).await?;
    }
    Ok(AuthInitResponse {
        process_id: result.process_id,
        mfa_required: result.mfa_required,
        mfa_type: result.mfa_type,
    })
}

pub async fn complete_auth(
    state: &AppState,
    process_id: &str,
    code: &str,
    member_id: i64,
) -> AppResult<SessionStatusResponse> {
    let response: SidecarSessionState = client(state)
        .post(
            "/complete",
            &json!({ "processId": process_id, "code": code }),
            AUTH_TIMEOUT,
            "Could not complete Linxea authentication",
            Some(SidecarErrorCode::InvalidOtp),
        )
        .await?;
    let plain_state = response
        .session_state
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            error(
                SidecarErrorCode::InvalidData,
                "Linxea did not return a session",
            )
        })?;
    store_session_and_queue(state, plain_state, member_id).await
}

pub async fn queue_sync(state: &AppState, member_id: i64) -> AppResult<SessionStatusResponse> {
    Ok(
        SessionStatusResponse::of(&template().queue(state, member_id, false).await?)
            .with_webview_supported(webview_supported(state)),
    )
}

async fn store_session_and_queue(
    state: &AppState,
    plain_state: String,
    member_id: i64,
) -> AppResult<SessionStatusResponse> {
    let session = template()
        .replace_and_queue(state, member_id, plain_state, false, |_| Ok(()))
        .await?;
    Ok(SessionStatusResponse::of(&session).with_webview_supported(webview_supported(state)))
}

pub async fn get_status(state: &AppState, member_id: i64) -> AppResult<SessionStatusResponse> {
    let supported = webview_supported(state);
    Ok(template()
        .find(state, member_id)
        .await?
        .map_or_else(SessionStatusResponse::inactive, |s| {
            SessionStatusResponse::of(&s)
        })
        .with_webview_supported(supported))
}

/// Whether a stored session was there to delete.
pub async fn clear_session(state: &AppState, member_id: i64) -> AppResult<bool> {
    let logins = webview_logins(state);
    let (removed, cancelled_webviews) = {
        let _write = logins.write.lock().await;
        let webviews = logins.forget_member(member_id);
        let removed = template().clear(state, member_id).await?;
        (removed, webviews)
    };
    for process_id in cancelled_webviews {
        cancel_bridge_webview(state, &process_id).await;
    }
    Ok(removed)
}

async fn cancel_bridge_webview(state: &AppState, process_id: &str) {
    if !webview_supported(state) || state.connectors.base_url(BANK.service_name()).is_empty() {
        return;
    }
    let http = SidecarHttp::new(state, BANK.service_name(), BANK.label());
    let path = format!("/webview/cancel/{process_id}");
    let _ = http
        .send::<Value>(http::Method::POST, &path, Some(&json!({})), WEBVIEW_TIMEOUT)
        .await;
}

pub async fn start_webview(state: &AppState, member_id: i64) -> AppResult<WebviewStartResponse> {
    require_webview(state)?;
    let logins = webview_logins(state);
    let generation = logins.generation(member_id);
    let started: BridgeWebviewStart = client(state)
        .post(
            "/webview/start",
            &json!({}),
            WEBVIEW_TIMEOUT,
            "Could not start the Linxea WebView login",
            None,
        )
        .await?;
    let Some(process_id) = started.process_id.filter(|s| clean(Some(s)).is_some()) else {
        return Err(error(
            SidecarErrorCode::UpstreamFormatChanged,
            "Linxea WebView bridge did not return a process identifier",
        )
        .into());
    };
    let status = started.status.unwrap_or_else(|| "WAITING_LOGIN".to_owned());
    let now = state.clock.now();
    let registered = {
        let _write = logins.write.lock().await;
        match logins.require_generation(member_id, generation) {
            Ok(()) => {
                logins.evict_expired(now);
                logins.maps().pending.insert(
                    process_id.clone(),
                    PendingWebview {
                        member_id,
                        deadline: now + chrono::Duration::seconds(WEBVIEW_ATTEMPT_SECONDS),
                        generation,
                    },
                );
                Ok(())
            }
            Err(err) => Err(err),
        }
    };
    if let Err(err) = registered {
        cancel_bridge_webview(state, &process_id).await;
        return Err(err.into());
    }
    Ok(WebviewStartResponse { process_id, status })
}

pub async fn poll_webview_status(
    state: &AppState,
    process_id: &str,
    member_id: i64,
) -> AppResult<WebviewStatusResponse> {
    require_webview(state)?;
    let logins = webview_logins(state);
    let current = logins.owned_attempt(process_id, member_id, state.clock.now())?;
    let c = client(state);
    let http = SidecarHttp::new(state, BANK.service_name(), BANK.label());
    let path = format!("/webview/status/{process_id}");
    let raw = http
        .send::<()>(http::Method::GET, &path, None, WEBVIEW_TIMEOUT)
        .await
        .map_err(|e| c.map_call_error("Could not read Linxea WebView status", e))?;
    if !raw.is_success() {
        let mapped = c.map_status("Could not read Linxea WebView status", &raw, None);
        if raw.status == http::StatusCode::GONE || raw.status == http::StatusCode::NOT_FOUND {
            logins.remove(process_id);
        }
        return Err(mapped.into());
    }
    let bridge: BridgeWebviewStatus = c
        .decode("Could not decode Linxea WebView status", &raw)?
        .ok_or_else(|| {
            c.coded(
                SidecarErrorCode::UpstreamUnavailable,
                "Could not decode Linxea WebView status",
            )
        })?;
    let status = bridge.status.unwrap_or_else(|| "FAILED".to_owned());
    match status.as_str() {
        "WAITING_LOGIN" | "FETCHING" => Ok(WebviewStatusResponse {
            status,
            error_code: None,
            session: None,
        }),
        "CANCELLED" => {
            logins.remove(process_id);
            Ok(WebviewStatusResponse {
                status,
                error_code: None,
                session: None,
            })
        }
        "FAILED" => {
            logins.remove(process_id);
            let error_code = Some(
                BANK.parse(bridge.error_code.as_deref())
                    .unwrap_or(SidecarErrorCode::UpstreamUnavailable),
            );
            Ok(WebviewStatusResponse {
                status,
                error_code,
                session: None,
            })
        }
        "DONE" => {
            let Some(plain_state) = bridge.session_state.filter(|s| clean(Some(s)).is_some())
            else {
                logins.remove(process_id);
                return Err(error(
                    SidecarErrorCode::InvalidData,
                    "Linxea WebView login did not return a session",
                )
                .into());
            };
            let tpl = template();
            let job = {
                let _write = logins.write.lock().await;
                logins.require_generation(member_id, current.generation)?;
                let still_pending = logins.maps().pending.remove(process_id);
                if still_pending.is_none() {
                    let session = get_status(state, member_id).await?;
                    return Ok(WebviewStatusResponse {
                        status: "SYNCING".to_owned(),
                        error_code: None,
                        session: Some(session),
                    });
                }
                let mut tx = state.db.begin().await?;
                let job = tpl
                    .insert_queued(
                        state,
                        &mut tx.as_handle(),
                        member_id,
                        plain_state,
                        false,
                        |_| Ok(()),
                    )
                    .await?;
                tx.commit().await?;
                job
            };
            let session = get_status(state, member_id).await?;
            tpl.submit(state, job)?;
            Ok(WebviewStatusResponse {
                status: "SYNCING".to_owned(),
                error_code: None,
                session: Some(session),
            })
        }
        _ => {
            logins.remove(process_id);
            Err(error(
                SidecarErrorCode::UpstreamFormatChanged,
                "Linxea WebView bridge returned an unexpected status",
            )
            .into())
        }
    }
}

pub async fn cancel_webview(
    state: &AppState,
    process_id: &str,
    member_id: i64,
) -> AppResult<WebviewCancelResponse> {
    require_webview(state)?;
    let logins = webview_logins(state);
    let removed = {
        let mut maps = logins.maps();
        if maps
            .pending
            .get(process_id)
            .is_some_and(|p| p.member_id == member_id)
        {
            maps.pending.remove(process_id);
            true
        } else {
            false
        }
    };
    if !removed {
        return Ok(WebviewCancelResponse { cancelled: false });
    }
    cancel_bridge_webview(state, process_id).await;
    Ok(WebviewCancelResponse { cancelled: true })
}

pub async fn recover_interrupted_syncs(state: &AppState) -> sqlx::Result<u64> {
    template().recover_interrupted(state).await
}

/// The daily scheduler's entry (track T8).
pub async fn resync_reporting(state: &AppState, member_id: i64) -> SourceSyncResult {
    template()
        .resync_reporting(
            member_id,
            "linxea",
            get_status(state, member_id),
            || async { queue_sync(state, member_id).await.map(|_| ()) },
        )
        .await
}

fn invalid(message: &str) -> SyncError {
    error(SidecarErrorCode::InvalidData, message)
}

fn incomplete(message: &str) -> SyncError {
    error(SidecarErrorCode::PortfolioIncomplete, message)
}

async fn prepare_accounts(
    state: &AppState,
    fetched: Vec<Option<AccountData>>,
) -> Result<Vec<PreparedAccount>, SyncError> {
    if fetched.is_empty() {
        return Err(incomplete("Linxea returned no complete contract"));
    }
    let mut external_ids = HashSet::new();
    let mut prepared = Vec::with_capacity(fetched.len());
    for account in fetched {
        let Some(account) = account.filter(|a| a.snapshot_complete) else {
            return Err(incomplete("Linxea returned an incomplete contract"));
        };
        let external_id = stable_external_id(account.external_id.as_deref())?;
        if !external_ids.insert(external_id.clone()) {
            return Err(invalid("Linxea returned duplicate contracts"));
        }
        let account_type = match account.account_type {
            Some(t @ (AccountType::Per | AccountType::AssuranceVie | AccountType::Pea)) => t,
            _ => {
                return Err(invalid("Linxea returned an unsupported contract type"));
            }
        };
        let (Some(balance_eur), Some(cash_balance), Some(raw_positions)) =
            (account.balance_eur, account.cash_balance, account.positions)
        else {
            return Err(invalid("Linxea returned incomplete contract values"));
        };
        let positions = prepare_positions(state, raw_positions).await?;
        let position_value: Decimal = positions.iter().map(|p| p.current_value_eur).sum();
        if !money_close(position_value, balance_eur - cash_balance) {
            return Err(incomplete("Linxea returned an incomplete contract"));
        }
        let invested_amount_eur = invested_amount(balance_eur, cash_balance, &positions);
        prepared.push(PreparedAccount {
            external_id,
            name: limit(account.name.as_deref(), 100, "Linxea account"),
            account_type,
            balance_eur,
            cash_balance,
            invested_amount_eur,
            positions,
        });
    }
    Ok(prepared)
}

async fn prepare_positions(
    state: &AppState,
    raw: Vec<Option<Position>>,
) -> Result<Vec<PreparedPosition>, SyncError> {
    let mut positions: Vec<PreparedPosition> = Vec::new();
    let mut isin_by_ticker = BTreeMap::new();
    for position in raw {
        let Some((position, quantity, current_value_eur)) = position.and_then(|p| {
            let (quantity, value) = (p.quantity?, p.current_value_eur?);
            Some((p, quantity, value))
        }) else {
            return Err(invalid("Linxea returned an incomplete position"));
        };
        if quantity.is_zero() {
            continue;
        }
        let currency = normalize_currency(position.quote_currency.as_deref())?;
        if position.current_price.is_some() && currency.is_none() {
            return Err(invalid(
                "Linxea returned a quote without its currency",
            ));
        }
        let ticker = resolve_ticker(state, &position, &mut isin_by_ticker).await?;
        let resolved = PreparedPosition {
            name: limit(position.label.as_deref(), 100, &ticker),
            ticker: ticker.clone(),
            isin: crate::ledger::model::normalize_isin(position.isin.as_deref()),
            quantity,
            average_buy_in_eur: position.buying_price_eur,
            current_price: position.current_price,
            quote_currency: currency,
            current_value_eur,
            pnl_eur: position.pnl_eur,
        };
        match positions.iter_mut().find(|p| p.ticker == ticker) {
            Some(existing) => *existing = merge_positions(existing, resolved)?,
            None => positions.push(resolved),
        }
    }
    state
        .connectors
        .seams
        .security_identities
        .record(&isin_by_ticker, state.clock.now())
        .await;
    positions.retain(|p| !p.quantity.is_zero());
    Ok(positions)
}

async fn resolve_ticker(
    state: &AppState,
    position: &Position,
    isin_by_ticker: &mut BTreeMap<String, String>,
) -> Result<String, SyncError> {
    let mut ticker = clean(position.symbol.as_deref());
    let isin = normalize_isin(position.isin.as_deref())?;
    if let Some(isin) = &isin {
        let resolved = state.connectors.seams.isin.resolve(isin).await;
        if !resolved.ticker.trim().is_empty() {
            ticker = Some(resolved.ticker.trim().to_owned());
        }
    }
    let too_long = |t: &Option<String>| t.as_ref().is_none_or(|t| t.encode_utf16().count() > 30);
    if too_long(&ticker)
        && let Some(isin) = isin.as_ref().filter(|i| i.len() <= 30)
    {
        ticker = Some(isin.clone());
    }
    if too_long(&ticker) {
        return Err(invalid(
            "Linxea returned an invalid instrument identifier",
        ));
    }
    let ticker = ticker.unwrap_or_default();
    if let Some(isin) = isin {
        isin_by_ticker.insert(ticker.clone(), isin);
    }
    Ok(ticker)
}

fn normalize_isin(raw: Option<&str>) -> Result<Option<String>, SyncError> {
    let Some(isin) = clean(raw) else {
        return Ok(None);
    };
    let isin = isin.to_uppercase();
    if !ISIN.is_match(&isin) {
        return Err(invalid("Linxea returned an invalid ISIN"));
    }
    Ok(Some(isin))
}

fn normalize_currency(raw: Option<&str>) -> Result<Option<String>, SyncError> {
    let Some(currency) = clean(raw) else {
        return Ok(None);
    };
    let currency = currency.to_uppercase();
    if !CURRENCY.is_match(&currency) {
        return Err(invalid("Linxea returned an invalid quote currency"));
    }
    Ok(Some(currency))
}

fn merge_positions(
    left: &PreparedPosition,
    right: PreparedPosition,
) -> Result<PreparedPosition, SyncError> {
    if left.quote_currency != right.quote_currency {
        return Err(invalid(
            "Linxea returned conflicting quote currencies",
        ));
    }
    let quantity = left.quantity + right.quantity;
    Ok(PreparedPosition {
        ticker: left.ticker.clone(),
        isin: left.isin.clone().or(right.isin),
        name: right.name,
        quantity,
        average_buy_in_eur: weighted_average(
            left.average_buy_in_eur,
            left.quantity,
            right.average_buy_in_eur,
            right.quantity,
            quantity,
        ),
        current_price: weighted_average(
            left.current_price,
            left.quantity,
            right.current_price,
            right.quantity,
            quantity,
        ),
        quote_currency: left.quote_currency.clone(),
        current_value_eur: left.current_value_eur + right.current_value_eur,
        pnl_eur: sum_complete(left.pnl_eur, right.pnl_eur),
    })
}

/// Cash plus each line's cost basis; the balance itself as soon as one line has none.
fn invested_amount(
    balance_eur: Decimal,
    cash_balance: Decimal,
    positions: &[PreparedPosition],
) -> Decimal {
    let mut invested = cash_balance;
    for position in positions {
        let cost_basis = match (position.average_buy_in_eur, position.pnl_eur) {
            (Some(average), _) => average * position.quantity,
            (None, Some(pnl)) => position.current_value_eur - pnl,
            (None, None) => return balance_eur,
        };
        invested += cost_basis;
    }
    invested
}

async fn upsert_account(
    state: &AppState,
    conn: &mut DbHandle<'_>,
    data: &PreparedAccount,
    member_id: i64,
    synced_at: DateTime<Utc>,
) -> JobResult<()> {
    let drafts: Vec<HoldingDraft> = data
        .positions
        .iter()
        .map(|p| HoldingDraft {
            ticker: p.ticker.clone(),
            isin: p.isin.clone(),
            name: Some(p.name.clone()),
            quantity: p.quantity,
            average_buy_in: p.average_buy_in_eur,
            current_price: p.current_price,
            quote_currency: p.quote_currency.clone(),
            provider_value_eur: Some(p.current_value_eur),
            provider_pnl_eur: p.pnl_eur,
            last_synced_at: Some(synced_at),
            ..HoldingDraft::default()
        })
        .collect();
    let account = PortfolioAccount {
        external_id: &data.external_id,
        name: &data.name,
        account_type: data.account_type,
        provider: PROVIDER,
        color: match data.account_type {
            AccountType::Per => "#f59e0b",
            AccountType::AssuranceVie => "#8b5cf6",
            _ => "#10b981",
        },
        balance_eur: data.balance_eur,
        cash_balance: Some(data.cash_balance),
        invested_eur: data.invested_amount_eur,
    };
    portfolio::write(state, conn, member_id, &account, &drafts, synced_at).await?;
    Ok(())
}

fn stable_external_id(raw: Option<&str>) -> Result<String, SyncError> {
    let cleaned = clean(raw)
        .ok_or_else(|| invalid("Linxea returned an invalid contract identifier"))?;
    let external_id = if cleaned.starts_with("linxea_") {
        cleaned
    } else {
        format!("linxea_{cleaned}")
    };
    if external_id.encode_utf16().count() > 100 {
        return Err(invalid(
            "Linxea returned an invalid contract identifier",
        ));
    }
    Ok(external_id)
}
