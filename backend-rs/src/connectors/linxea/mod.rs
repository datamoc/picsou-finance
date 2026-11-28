//! LinxeaController (track T6): login with an e-mailed code (or native Desktop WebView), then a
//! queued import of the PER, assurance-vie and PEA contracts with their supports.

pub mod export_parser;
mod sync;

use axum::Json as AxumJson;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use regex::Regex;
use serde::Deserialize;
use std::sync::LazyLock;

pub use sync::{recover_interrupted_syncs, resync_reporting};

use super::sidecar::keypad::no_store;
use super::sidecar::web::{AuthInitResponse, SessionStatusResponse, consume};
use crate::AppState;
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::http::client_ip::ClientIp;
use crate::http::json::{Errors, Json, Validate};
use crate::http::rate_limit::policies;
use crate::routes::Routes;

pub fn register(routes: &mut Routes) {
    routes
        .post("/api/linxea/auth/initiate", initiate)
        .post("/api/linxea/auth/complete", complete)
        .post("/api/linxea/webview/start", webview_start)
        .get("/api/linxea/webview/status/{processId}", webview_status)
        .post("/api/linxea/webview/cancel/{processId}", webview_cancel)
        .post("/api/linxea/sync", sync_now)
        .get("/api/linxea/status", status)
        .delete("/api/linxea/session", clear);
}

const INVALID_WEBVIEW_ID: &str = "The Linxea request is not valid";

fn validate_process_id(process_id: &str) -> AppResult<()> {
    if process_id.is_empty()
        || process_id.len() > 128
        || !process_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(AppError::BadRequest(INVALID_WEBVIEW_ID.to_owned()));
    }
    Ok(())
}

const RATE_LIMITED: &str =
    "Too many Linxea authentication attempts. Please wait before retrying.";
static SIX_DIGITS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{6}$").expect("regex"));
static EMAIL_OR_IDENTIFIER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[^\s@]+@[^\s@]+\.[^\s@]+$|^[A-Za-z0-9._-]{1,100}$").expect("regex")
});

#[derive(Debug, Deserialize)]
struct InitiateRequest {
    login: Option<String>,
    password: Option<String>,
}

impl Validate for InitiateRequest {
    fn validate(&self, e: &mut Errors) {
        let (login, password) = (self.login.as_deref(), self.password.as_deref());
        e.not_blank("login", login)
            .size("login", login, 0, 200)
            .pattern_default("login", login, &EMAIL_OR_IDENTIFIER, r"[^\s@]+@[^\s@]+\.[^\s@]+");
        e.not_blank("password", password)
            .size("password", password, 0, 200);
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompleteRequest {
    process_id: Option<String>,
    code: Option<String>,
}

impl Validate for CompleteRequest {
    fn validate(&self, e: &mut Errors) {
        let (process_id, code) = (self.process_id.as_deref(), self.code.as_deref());
        e.not_blank("processId", process_id)
            .size("processId", process_id, 0, 100);
        e.not_blank("code", code)
            .pattern_default("code", code, &SIX_DIGITS, r"\d{6}");
    }
}

async fn initiate(
    State(state): State<AppState>,
    auth: Auth,
    ip: ClientIp,
    Json(req): Json<InitiateRequest>,
) -> AppResult<AxumJson<AuthInitResponse>> {
    consume(&state, &policies::LINXEA_AUTH, &ip, RATE_LIMITED)?;
    let login = req.login.unwrap_or_default();
    let password = req.password.unwrap_or_default();
    Ok(AxumJson(
        sync::initiate_auth(&state, &login, &password, auth.member_id).await?,
    ))
}

async fn complete(
    State(state): State<AppState>,
    auth: Auth,
    ip: ClientIp,
    Json(req): Json<CompleteRequest>,
) -> AppResult<AxumJson<SessionStatusResponse>> {
    consume(&state, &policies::LINXEA_AUTH, &ip, RATE_LIMITED)?;
    let process_id = req.process_id.unwrap_or_default();
    let code = req.code.unwrap_or_default();
    Ok(AxumJson(
        sync::complete_auth(&state, &process_id, &code, auth.member_id).await?,
    ))
}

async fn webview_start(
    State(state): State<AppState>,
    auth: Auth,
    ip: ClientIp,
) -> AppResult<Response> {
    consume(&state, &policies::LINXEA_WEBVIEW, &ip, RATE_LIMITED)?;
    Ok(no_store(sync::start_webview(&state, auth.member_id).await?))
}

async fn webview_status(
    State(state): State<AppState>,
    auth: Auth,
    Path(process_id): Path<String>,
) -> AppResult<Response> {
    validate_process_id(&process_id)?;
    Ok(no_store(
        sync::poll_webview_status(&state, &process_id, auth.member_id).await?,
    ))
}

async fn webview_cancel(
    State(state): State<AppState>,
    auth: Auth,
    Path(process_id): Path<String>,
) -> AppResult<Response> {
    validate_process_id(&process_id)?;
    Ok(no_store(
        sync::cancel_webview(&state, &process_id, auth.member_id).await?,
    ))
}

async fn sync_now(State(state): State<AppState>, auth: Auth) -> AppResult<Response> {
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
