//! BoursoController (track T6): login with the customer number and an app-push validation, then a
//! queued import of the current accounts, livrets, securities accounts and life contracts.

mod sync;

use crate::connectors::sidecar::template::error;
use crate::ledger::sidecar::{SidecarBank, SidecarErrorCode};
use crate::native_sidecars::{is_native_url, natixis::PasskeyBridge};
use axum::Json as AxumJson;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

pub use sync::{recover_interrupted_syncs, resync_reporting};

use super::sidecar::web::{AuthInitResponse, SessionStatusResponse, consume};
use crate::AppState;
use crate::auth::Auth;
use crate::error::AppResult;
use crate::http::client_ip::ClientIp;
use crate::http::json::{Errors, Json, Validate};
use crate::http::rate_limit::policies;
use crate::routes::Routes;

pub fn register(routes: &mut Routes) {
    routes
        .post("/api/bourso/auth/initiate", initiate)
        .post("/api/bourso/auth/complete", complete)
        .get("/api/bourso/auth/passkey", passkey_availability)
        .post("/api/bourso/auth/passkey", passkey_login)
        .post("/api/bourso/sync", sync_now)
        .get("/api/bourso/status", status)
        .delete("/api/bourso/session", clear);
}

const AUTH_RATE_LIMITED: &str =
    "Too many BoursoBank authentication attempts. Please wait before retrying.";
const SYNC_RATE_LIMITED: &str =
    "Too many BoursoBank synchronization requests. Please wait before retrying.";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitiateRequest {
    customer_id: Option<String>,
    password: Option<String>,
}

impl Validate for InitiateRequest {
    fn validate(&self, e: &mut Errors) {
        let (customer_id, password) = (self.customer_id.as_deref(), self.password.as_deref());
        e.not_blank("customerId", customer_id)
            .size("customerId", customer_id, 0, 100);
        e.not_blank("password", password)
            .size("password", password, 0, 100);
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PasskeyRequest {
    customer_id: Option<String>,
}

impl Validate for PasskeyRequest {
    fn validate(&self, e: &mut Errors) {
        let id = self.customer_id.as_deref();
        e.not_blank("customerId", id).size("customerId", id, 8, 8);
    }
}

#[derive(Serialize)]
struct PasskeyAvailability {
    available: bool,
}

/// Whether a passkey prompt can be shown: the in-process connector, next to a desktop shell.
fn offers_passkey(state: &AppState) -> bool {
    is_native_url(
        state
            .connectors
            .base_url(SidecarBank::Bourso.service_name()),
    ) && PasskeyBridge::from_config(&state.config).is_some()
}

async fn passkey_availability(
    State(state): State<AppState>,
    _auth: Auth,
) -> AxumJson<PasskeyAvailability> {
    AxumJson(PasskeyAvailability {
        available: offers_passkey(&state),
    })
}

async fn passkey_login(
    State(state): State<AppState>,
    auth: Auth,
    ip: ClientIp,
    Json(req): Json<PasskeyRequest>,
) -> AppResult<AxumJson<SessionStatusResponse>> {
    consume(&state, &policies::BOURSO_AUTH, &ip, AUTH_RATE_LIMITED)?;
    if !offers_passkey(&state) {
        return Err(error(
            SidecarErrorCode::UpstreamUnavailable,
            "Passkey sign-in needs Picsou Desktop",
        )
        .into());
    }
    Ok(AxumJson(
        sync::passkey_auth(
            &state,
            req.customer_id.as_deref().unwrap_or_default(),
            auth.member_id,
        )
        .await?,
    ))
}

/// No code: the app push is the only second factor this connector drives.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompleteRequest {
    process_id: Option<String>,
}

impl Validate for CompleteRequest {
    fn validate(&self, e: &mut Errors) {
        let process_id = self.process_id.as_deref();
        e.not_blank("processId", process_id)
            .size("processId", process_id, 0, 100);
    }
}

async fn initiate(
    State(state): State<AppState>,
    auth: Auth,
    ip: ClientIp,
    Json(req): Json<InitiateRequest>,
) -> AppResult<AxumJson<AuthInitResponse>> {
    consume(&state, &policies::BOURSO_AUTH, &ip, AUTH_RATE_LIMITED)?;
    let customer_id = req.customer_id.unwrap_or_default();
    let password = req.password.unwrap_or_default();
    Ok(AxumJson(
        sync::initiate_auth(&state, &customer_id, &password, auth.member_id).await?,
    ))
}

/// Blocks while the customer approves the push on their phone, up to two minutes.
async fn complete(
    State(state): State<AppState>,
    auth: Auth,
    ip: ClientIp,
    Json(req): Json<CompleteRequest>,
) -> AppResult<AxumJson<SessionStatusResponse>> {
    consume(&state, &policies::BOURSO_AUTH, &ip, AUTH_RATE_LIMITED)?;
    let process_id = req.process_id.unwrap_or_default();
    Ok(AxumJson(
        sync::complete_auth(&state, &process_id, auth.member_id).await?,
    ))
}

/// Throttled like every other sync entry point: queueing takes a row lock and decrypts the session.
async fn sync_now(State(state): State<AppState>, auth: Auth, ip: ClientIp) -> AppResult<Response> {
    consume(&state, &policies::SYNC, &ip, SYNC_RATE_LIMITED)?;
    let status = sync::queue_sync(&state, auth.member_id).await?;
    Ok((StatusCode::ACCEPTED, AxumJson(status)).into_response())
}

async fn status(
    State(state): State<AppState>,
    auth: Auth,
) -> AppResult<AxumJson<SessionStatusResponse>> {
    Ok(AxumJson(sync::get_status(&state, auth.member_id).await?))
}

async fn clear(State(state): State<AppState>, auth: Auth) -> AppResult<StatusCode> {
    sync::clear_session(&state, auth.member_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
