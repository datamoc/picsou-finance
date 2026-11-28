//! Linxea native WebView connector (zero Docker, zero Python, pure Rust on Windows/macOS/Linux).
//!
//! Opens a real OS WebView (`WebView2` on Windows, `WKWebView` on macOS, `WebKitGTK` on Linux)
//! directly on `https://espaceclient.linxea.com/authentification` so the user authenticates
//! naturally (e-mail, password when prompted, and 6-digit e-mail verification code). Picsou
//! never sees credentials.
//!
//! Once the WebView lands on an authenticated client-space route (e.g. `/epargne` or
//! `/epargne/contrat/<uuid>`) and `localStorage` holds a valid `oidc.user:*` token, the bridge
//! calls `https://contracts-api.linxea.com/api/v2/users/{userId}/contracts` and each contract's
//! `.../export` endpoint via `reqwest`, parses every XLSX workbook directly in Rust via
//! `picsou::connectors::linxea::export_parser`, caches the snapshot for the immediate backend
//! sync, and closes the login window cleanly.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::http::StatusCode;
use picsou::connectors::linxea::export_parser::{self, AccountPayload};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{
    AppHandle, Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent,
};
use tokio::sync::oneshot;
use uuid::Uuid;

use super::bnp::{
    random_hex, sha256_hex, unwrap_eval_string, ApiError, WebviewStartResponse,
    WebviewStatusResponse,
};
use super::{
    check_navigation_scheme, document_ready_complete, enforce_main_frame_url,
    is_domain_or_subdomain, read_capped_bytes, read_capped_text, ReadBodyError,
};

const LOGIN_URL: &str = "https://espaceclient.linxea.com/authentification";
const APP_HOST: &str = "espaceclient.linxea.com";
const CONTRACTS_API: &str = "https://contracts-api.linxea.com";
const WINDOW_TITLE: &str = "Connexion — Linxea";
const XLSX_ACCEPT: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet, application/octet-stream, */*";

const LOGIN_TTL: Duration = Duration::from_secs(300);
const POLL_INTERVAL: Duration = Duration::from_millis(600);
const SNAPSHOT_CACHE_TTL: Duration = Duration::from_secs(180);
const MAX_PROCESSES: usize = 4;
const MAX_CACHE_ENTRIES: usize = 8;
const MAX_SESSION_STATE_BYTES: usize = 120_000;
const MAX_CONTRACTS_JSON_BYTES: usize = 4_000_000;
const MAX_EXPORT_BYTES: usize = 16 * 1024 * 1024;
const MAX_CONTRACTS: usize = 100;
const CONTRACTS_PAGE_SIZE: usize = 100;

#[derive(Debug, Clone)]
enum ProcessStatus {
    WaitingLogin,
    Fetching,
    Done {
        session_state: String,
        completed_at: Instant,
    },
    Cancelled,
    Failed {
        error_code: &'static str,
    },
}

struct ProcessEntry {
    status: ProcessStatus,
    window_label: String,
    created_at: Instant,
}

struct CachedSnapshot {
    expires_at: Instant,
    accounts: Vec<AccountPayload>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LinxeaSessionEnvelope {
    user_id: String,
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    authority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    fallback_contract_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    desktop_only: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    nonce: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExtractedOidc {
    storage_key: String,
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_at: Option<u64>,
    user_id: String,
}

#[derive(Debug, Clone)]
struct ContractSummary {
    contract_id: String,
    contract_number: String,
    product_name: String,
    contract_type_code: String,
    is_active: bool,
}

pub struct LinxeaWebviewManager {
    processes: Mutex<HashMap<String, ProcessEntry>>,
    cache: Mutex<HashMap<String, CachedSnapshot>>,
    http: reqwest::Client,
}

impl LinxeaWebviewManager {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(45))
            .build()
            .expect("reqwest client");
        Self {
            processes: Mutex::new(HashMap::new()),
            cache: Mutex::new(HashMap::new()),
            http,
        }
    }

    pub async fn start_login(
        self: &Arc<Self>,
        app: AppHandle,
    ) -> Result<WebviewStartResponse, ApiError> {
        let stale_labels: Vec<String> = {
            let mut procs = self.processes.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            procs.retain(|_, p| {
                now.duration_since(p.created_at) < LOGIN_TTL + Duration::from_secs(60)
            });
            let mut labels = Vec::new();
            for entry in procs.values_mut() {
                if matches!(
                    entry.status,
                    ProcessStatus::WaitingLogin | ProcessStatus::Fetching
                ) {
                    entry.status = ProcessStatus::Cancelled;
                    labels.push(entry.window_label.clone());
                }
            }
            while procs.len() >= MAX_PROCESSES {
                if let Some(oldest) = procs
                    .iter()
                    .min_by_key(|(_, p)| p.created_at)
                    .map(|(k, _)| k.clone())
                {
                    procs.remove(&oldest);
                } else {
                    break;
                }
            }
            labels
        };
        for label in stale_labels {
            if let Some(win) = app.get_webview_window(&label) {
                let _ = win.destroy();
            }
        }

        let process_id = Uuid::new_v4().simple().to_string();
        let window_label = format!("picsou-linxea-{process_id}");
        let entry_url = Url::parse(LOGIN_URL)
            .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR"))?;

        {
            let mut procs = self.processes.lock().unwrap_or_else(|e| e.into_inner());
            procs.insert(
                process_id.clone(),
                ProcessEntry {
                    status: ProcessStatus::WaitingLogin,
                    window_label: window_label.clone(),
                    created_at: Instant::now(),
                },
            );
        }

        let app_for_build = app.clone();
        let label_for_build = window_label.clone();
        let login_url_for_load = entry_url.clone();
        let manager_for_load = Arc::clone(self);
        let pid_for_load = process_id.clone();
        let manager_for_close = Arc::clone(self);
        let pid_for_close = process_id.clone();

        let window = tokio::task::spawn_blocking(move || {
            let win = WebviewWindowBuilder::new(
                &app_for_build,
                &label_for_build,
                WebviewUrl::External(entry_url),
            )
            .title(WINDOW_TITLE)
            .inner_size(1040.0, 780.0)
            .min_inner_size(720.0, 560.0)
            .center()
            .incognito(true)
            .visible(true)
            .focused(true)
            .on_navigation(|url| check_navigation_scheme("LINXEA", url))
            .on_page_load(move |window, payload| {
                enforce_main_frame_url(
                    "LINXEA",
                    &window,
                    payload.url(),
                    &login_url_for_load,
                    is_allowed_main_frame_url,
                    || manager_for_load.mark_failed(&pid_for_load, "UPSTREAM_UNAVAILABLE"),
                );
            })
            .build()?;
            win.on_window_event(move |event| {
                if matches!(
                    event,
                    WindowEvent::CloseRequested { .. } | WindowEvent::Destroyed
                ) {
                    manager_for_close.mark_cancelled_if_active(&pid_for_close);
                }
            });
            Ok::<WebviewWindow, tauri::Error>(win)
        })
        .await
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR"))?
        .map_err(|_| {
            self.mark_failed(&process_id, "UPSTREAM_UNAVAILABLE");
            ApiError::new(StatusCode::BAD_GATEWAY, "UPSTREAM_UNAVAILABLE")
        })?;

        tracing::info!("Linxea WebView login opened");

        let manager = Arc::clone(self);
        let pid = process_id.clone();
        tokio::spawn(async move {
            manager.watch_login(pid, window).await;
        });

        Ok(WebviewStartResponse {
            process_id,
            status: "WAITING_LOGIN",
        })
    }

    pub fn get_status(&self, process_id: &str) -> Result<WebviewStatusResponse, ApiError> {
        let mut procs = self.processes.lock().unwrap_or_else(|e| e.into_inner());
        procs.retain(|_, p| {
            !matches!(
                p.status,
                ProcessStatus::Done { completed_at, .. } if completed_at.elapsed() >= SNAPSHOT_CACHE_TTL
            )
        });
        if matches!(
            procs.get(process_id).map(|e| &e.status),
            Some(ProcessStatus::Done { .. })
        ) {
            if let Some(ProcessEntry {
                status: ProcessStatus::Done { session_state, .. },
                ..
            }) = procs.remove(process_id)
            {
                return Ok(WebviewStatusResponse {
                    status: "DONE",
                    session_state: Some(session_state),
                    error_code: None,
                });
            }
        }
        let Some(entry) = procs.get_mut(process_id) else {
            return Err(ApiError::new(StatusCode::GONE, "AUTH_ATTEMPT_EXPIRED"));
        };
        if matches!(entry.status, ProcessStatus::WaitingLogin)
            && entry.created_at.elapsed() >= LOGIN_TTL
        {
            entry.status = ProcessStatus::Failed {
                error_code: "AUTH_ATTEMPT_EXPIRED",
            };
        }
        Ok(match &entry.status {
            ProcessStatus::WaitingLogin => WebviewStatusResponse {
                status: "WAITING_LOGIN",
                session_state: None,
                error_code: None,
            },
            ProcessStatus::Fetching => WebviewStatusResponse {
                status: "FETCHING",
                session_state: None,
                error_code: None,
            },
            ProcessStatus::Done { .. } => unreachable!("handled above"),
            ProcessStatus::Cancelled => WebviewStatusResponse {
                status: "CANCELLED",
                session_state: None,
                error_code: None,
            },
            ProcessStatus::Failed { error_code } => WebviewStatusResponse {
                status: "FAILED",
                session_state: None,
                error_code: Some((*error_code).to_owned()),
            },
        })
    }

    pub fn cancel(&self, app: &AppHandle, process_id: &str) -> bool {
        let label = {
            let mut procs = self.processes.lock().unwrap_or_else(|e| e.into_inner());
            let Some(entry) = procs.get_mut(process_id) else {
                return false;
            };
            if matches!(entry.status, ProcessStatus::Done { .. }) {
                let removed = procs.remove(process_id).expect("entry just checked");
                removed.window_label
            } else {
                if matches!(
                    entry.status,
                    ProcessStatus::WaitingLogin | ProcessStatus::Fetching
                ) {
                    entry.status = ProcessStatus::Cancelled;
                }
                entry.window_label.clone()
            }
        };
        if let Some(win) = app.get_webview_window(&label) {
            let _ = win.destroy();
        }
        true
    }

    pub fn close_all(&self, app: &AppHandle) {
        let labels: Vec<String> = {
            let mut procs = self.processes.lock().unwrap_or_else(|e| e.into_inner());
            procs.drain().map(|(_, entry)| entry.window_label).collect()
        };
        for label in labels {
            if let Some(win) = app.get_webview_window(&label) {
                let _ = win.destroy();
            }
        }
    }

    fn mark_cancelled_if_active(&self, process_id: &str) {
        let mut procs = self.processes.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = procs.get_mut(process_id) {
            if matches!(
                entry.status,
                ProcessStatus::WaitingLogin | ProcessStatus::Fetching
            ) {
                entry.status = ProcessStatus::Cancelled;
                tracing::info!("Linxea WebView login cancelled by user");
            }
        }
    }

    fn mark_fetching(&self, process_id: &str) -> bool {
        let mut procs = self.processes.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = procs.get_mut(process_id) else {
            return false;
        };
        if matches!(entry.status, ProcessStatus::WaitingLogin) {
            entry.status = ProcessStatus::Fetching;
            true
        } else {
            false
        }
    }

    fn mark_done(&self, process_id: &str, session_state: String) -> bool {
        let mut procs = self.processes.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = procs.get_mut(process_id) {
            if matches!(entry.status, ProcessStatus::Fetching) {
                entry.status = ProcessStatus::Done {
                    session_state,
                    completed_at: Instant::now(),
                };
                return true;
            }
        }
        false
    }

    fn mark_failed(&self, process_id: &str, error_code: &'static str) {
        let mut procs = self.processes.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = procs.get_mut(process_id) {
            if matches!(
                entry.status,
                ProcessStatus::WaitingLogin | ProcessStatus::Fetching
            ) {
                entry.status = ProcessStatus::Failed { error_code };
            }
        }
    }

    fn is_waiting(&self, process_id: &str) -> bool {
        let procs = self.processes.lock().unwrap_or_else(|e| e.into_inner());
        procs
            .get(process_id)
            .is_some_and(|e| matches!(e.status, ProcessStatus::WaitingLogin))
    }

    fn remember_snapshot(&self, session_state: &str, accounts: Vec<AccountPayload>) {
        let key = sha256_hex(session_state);
        let now = Instant::now();
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.retain(|_, v| v.expires_at > now);
        while cache.len() >= MAX_CACHE_ENTRIES {
            if let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, v)| v.expires_at)
                .map(|(k, _)| k.clone())
            {
                cache.remove(&oldest);
            } else {
                break;
            }
        }
        cache.insert(
            key,
            CachedSnapshot {
                expires_at: now + SNAPSHOT_CACHE_TTL,
                accounts,
            },
        );
    }

    fn take_cached_snapshot(&self, session_state: &str) -> Option<Vec<AccountPayload>> {
        let key = sha256_hex(session_state);
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let entry = cache.remove(&key)?;
        if entry.expires_at > Instant::now() {
            Some(entry.accounts)
        } else {
            None
        }
    }

    async fn watch_login(self: Arc<Self>, process_id: String, window: WebviewWindow) {
        let deadline = Instant::now() + LOGIN_TTL;
        loop {
            if !self.is_waiting(&process_id) {
                let _ = window.destroy();
                return;
            }
            if Instant::now() >= deadline {
                tracing::warn!("Linxea WebView login timed out");
                self.mark_failed(&process_id, "AUTH_ATTEMPT_EXPIRED");
                let _ = window.destroy();
                return;
            }

            let current_url = match window.url() {
                Ok(url) => url,
                Err(_) => {
                    self.mark_cancelled_if_active(&process_id);
                    return;
                }
            };

            if on_authenticated_page(&current_url) && document_ready_complete(&window).await {
                if let Some(oidc) = read_oidc_from_webview(&window).await {
                    if !is_safe_path_segment(&oidc.user_id) || !token_currently_valid(oidc.expires_at)
                    {
                        tokio::time::sleep(POLL_INTERVAL).await;
                        continue;
                    }
                    if !self.mark_fetching(&process_id) {
                        let _ = window.destroy();
                        return;
                    }
                    tracing::info!("Linxea WebView authenticated, fetching contract exports");
                    let url_contract = contract_id_from_url(&current_url);
                    let fallback_ids: Vec<String> = url_contract.into_iter().collect();
                    match self
                        .fetch_accounts_with_token(&oidc.user_id, &oidc.access_token, &fallback_ids)
                        .await
                    {
                        Ok(accounts) => {
                            let (authority, client_id) = parse_oidc_key(&oidc.storage_key);
                            let collected_ids: Vec<String> = accounts
                                .iter()
                                .filter_map(|a| a.external_id.strip_prefix("linxea_").map(str::to_owned))
                                .collect();
                            let envelope = LinxeaSessionEnvelope {
                                user_id: oidc.user_id,
                                access_token: oidc.access_token,
                                refresh_token: oidc.refresh_token,
                                expires_at: oidc.expires_at,
                                authority,
                                client_id,
                                fallback_contract_ids: if collected_ids.is_empty() {
                                    fallback_ids
                                } else {
                                    collected_ids
                                },
                                desktop_only: Some(true),
                                nonce: Some(random_hex(16)),
                            };
                            match encode_session(&envelope) {
                                Ok(session_state) => {
                                    self.remember_snapshot(&session_state, accounts);
                                    if self.mark_done(&process_id, session_state) {
                                        let mgr = Arc::clone(&self);
                                        let pid = process_id.clone();
                                        tokio::spawn(async move {
                                            tokio::time::sleep(SNAPSHOT_CACHE_TTL).await;
                                            let mut procs = mgr
                                                .processes
                                                .lock()
                                                .unwrap_or_else(|e| e.into_inner());
                                            if matches!(
                                                procs.get(&pid).map(|e| &e.status),
                                                Some(ProcessStatus::Done { .. })
                                            ) {
                                                procs.remove(&pid);
                                            }
                                        });
                                    }
                                    tracing::info!(
                                        "Linxea WebView snapshot ready, closing login window"
                                    );
                                }
                                Err(err) => {
                                    tracing::warn!(
                                        "Linxea session encoding failed (code={})",
                                        err.detail
                                    );
                                    self.mark_failed(&process_id, err.detail);
                                }
                            }
                        }
                        Err(err) => {
                            tracing::warn!("Linxea WebView snapshot failed (code={})", err.detail);
                            self.mark_failed(&process_id, err.detail);
                        }
                    }
                    let _ = window.destroy();
                    return;
                }
            }

            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Serves `POST /linxea/accounts`: reuses the immediate post-login snapshot when present,
    /// otherwise calls `contracts-api.linxea.com` directly with the stored bearer token (or
    /// refreshes it through OIDC `refresh_token` when expired).
    pub async fn accounts(&self, session_state: &str) -> Result<Vec<AccountPayload>, ApiError> {
        let mut envelope = decode_session_state(session_state)?;
        if let Some(accounts) = self.take_cached_snapshot(session_state) {
            return Ok(accounts);
        }
        if !is_safe_path_segment(&envelope.user_id) || envelope.access_token.trim().is_empty() {
            return Err(ApiError::new(StatusCode::UNAUTHORIZED, "SESSION_EXPIRED"));
        }

        if !token_currently_valid(envelope.expires_at) {
            if let Some(refreshed) = self.try_refresh_token(&envelope).await {
                envelope.access_token = refreshed;
            } else {
                return Err(ApiError::new(StatusCode::UNAUTHORIZED, "SESSION_EXPIRED"));
            }
        }

        match self
            .fetch_accounts_with_token(
                &envelope.user_id,
                &envelope.access_token,
                &envelope.fallback_contract_ids,
            )
            .await
        {
            Ok(accounts) => Ok(accounts),
            Err(err) if err.detail == "SESSION_EXPIRED" => {
                if let Some(refreshed) = self.try_refresh_token(&envelope).await {
                    self.fetch_accounts_with_token(
                        &envelope.user_id,
                        &refreshed,
                        &envelope.fallback_contract_ids,
                    )
                    .await
                } else {
                    Err(err)
                }
            }
            Err(err) => Err(err),
        }
    }

    async fn try_refresh_token(&self, envelope: &LinxeaSessionEnvelope) -> Option<String> {
        let refresh_token = envelope.refresh_token.as_deref().filter(|s| !s.is_empty())?;
        let authority = envelope.authority.as_deref().filter(|s| !s.is_empty())?;
        let client_id = envelope.client_id.as_deref().filter(|s| !s.is_empty())?;

        let authority_url = Url::parse(authority).ok()?;
        if !is_allowed_main_frame_url(&authority_url) {
            return None;
        }
        let discovery_url = format!(
            "{}/.well-known/openid-configuration",
            authority.trim_end_matches('/')
        );
        let disc_resp = self
            .http
            .get(&discovery_url)
            .header("Accept", "application/json")
            .send()
            .await
            .ok()?;
        if disc_resp.status() != StatusCode::OK {
            return None;
        }
        let disc_text = read_capped_text(disc_resp, 256_000).await.ok()?;
        let disc_json: Value = serde_json::from_str(&disc_text).ok()?;
        let token_endpoint = disc_json.get("token_endpoint")?.as_str()?;
        let token_url = Url::parse(token_endpoint).ok()?;
        if !is_allowed_main_frame_url(&token_url) {
            return None;
        }

        let token_resp = self
            .http
            .post(token_endpoint)
            .header("Accept", "application/json")
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", client_id),
                ("refresh_token", refresh_token),
            ])
            .send()
            .await
            .ok()?;
        if token_resp.status() != StatusCode::OK {
            return None;
        }
        let body = read_capped_text(token_resp, 256_000).await.ok()?;
        let json: Value = serde_json::from_str(&body).ok()?;
        json.get("access_token")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    }

    async fn fetch_accounts_with_token(
        &self,
        user_id: &str,
        token: &str,
        fallback_contract_ids: &[String],
    ) -> Result<Vec<AccountPayload>, ApiError> {
        let mut contracts = match self.fetch_contracts(user_id, token).await {
            Ok(list) => list,
            Err(err) if err.detail == "SESSION_EXPIRED" => return Err(err),
            Err(err) if !fallback_contract_ids.is_empty() => {
                tracing::warn!(
                    code = err.detail,
                    "Linxea contracts listing failed; falling back to known contract ids"
                );
                Vec::new()
            }
            Err(err) => return Err(err),
        };

        if contracts.is_empty() && !fallback_contract_ids.is_empty() {
            contracts = fallback_contract_ids
                .iter()
                .filter(|id| is_safe_path_segment(id))
                .map(|id| ContractSummary {
                    contract_id: id.clone(),
                    contract_number: String::new(),
                    product_name: String::new(),
                    contract_type_code: String::new(),
                    is_active: true,
                })
                .collect();
        }

        let mut payloads = Vec::new();
        for contract in contracts {
            if !contract.is_active || !is_safe_path_segment(&contract.contract_id) {
                continue;
            }
            if contract.contract_type_code == "RealEstate" {
                // SCPI contracts do not use the savings-support XLSX format.
                continue;
            }
            let bytes = self
                .fetch_export(user_id, &contract.contract_id, token)
                .await?;
            match export_parser::parse_export(
                &bytes,
                &contract.contract_id,
                &contract.contract_number,
                &contract.product_name,
            ) {
                Ok(account) => payloads.push(account),
                Err(err) => {
                    tracing::warn!(
                        contract_type = %contract.contract_type_code,
                        reason = %err,
                        "Linxea contract export was not readable"
                    );
                }
            }
        }

        if payloads.is_empty() {
            return Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                "PORTFOLIO_INCOMPLETE",
            ));
        }
        Ok(payloads)
    }

    async fn fetch_contracts(
        &self,
        user_id: &str,
        token: &str,
    ) -> Result<Vec<ContractSummary>, ApiError> {
        let mut all = Vec::new();
        for page in 1..=5 {
            let url = format!(
                "{CONTRACTS_API}/api/v2/users/{user_id}/contracts?itemsPerPage={CONTRACTS_PAGE_SIZE}&page={page}"
            );
            let resp = self
                .http
                .get(&url)
                .header("Authorization", format!("Bearer {token}"))
                .header("Accept", "application/json")
                .send()
                .await
                .map_err(|_| {
                    ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "UPSTREAM_UNAVAILABLE")
                })?;

            let status = resp.status();
            if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                return Err(ApiError::new(StatusCode::UNAUTHORIZED, "SESSION_EXPIRED"));
            }
            if status != StatusCode::OK {
                tracing::warn!(status = status.as_u16(), "Linxea contracts call failed");
                return Err(ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "UPSTREAM_UNAVAILABLE",
                ));
            }
            let text = read_capped_text(resp, MAX_CONTRACTS_JSON_BYTES)
                .await
                .map_err(|e| match e {
                    ReadBodyError::Transport => {
                        ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "UPSTREAM_UNAVAILABLE")
                    }
                    ReadBodyError::TooLarge => {
                        ApiError::new(StatusCode::BAD_GATEWAY, "UPSTREAM_FORMAT_CHANGED")
                    }
                })?;
            let chunk = parse_contracts_response(&text)?;
            let count = chunk.len();
            all.extend(chunk);
            if count < CONTRACTS_PAGE_SIZE || all.len() >= MAX_CONTRACTS {
                break;
            }
        }
        all.truncate(MAX_CONTRACTS);
        Ok(all)
    }

    async fn fetch_export(
        &self,
        user_id: &str,
        contract_id: &str,
        token: &str,
    ) -> Result<Vec<u8>, ApiError> {
        let url = format!("{CONTRACTS_API}/api/v2/users/{user_id}/contracts/{contract_id}/export");
        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", XLSX_ACCEPT)
            .send()
            .await
            .map_err(|_| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "UPSTREAM_UNAVAILABLE"))?;

        let status = resp.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(ApiError::new(StatusCode::UNAUTHORIZED, "SESSION_EXPIRED"));
        }
        if status != StatusCode::OK {
            tracing::warn!(status = status.as_u16(), "Linxea export call failed");
            return Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                "UPSTREAM_UNAVAILABLE",
            ));
        }
        let bytes = read_capped_bytes(resp, MAX_EXPORT_BYTES)
            .await
            .map_err(|e| match e {
                ReadBodyError::Transport => {
                    ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "UPSTREAM_UNAVAILABLE")
                }
                ReadBodyError::TooLarge => {
                    ApiError::new(StatusCode::BAD_GATEWAY, "UPSTREAM_FORMAT_CHANGED")
                }
            })?;
        if bytes.is_empty() {
            return Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                "PORTFOLIO_INCOMPLETE",
            ));
        }
        Ok(bytes)
    }
}

// ─── WebView OIDC reader ─────────────────────────────────────────────────────

async fn read_oidc_from_webview(window: &WebviewWindow) -> Option<ExtractedOidc> {
    let (tx, rx) = oneshot::channel::<String>();
    let sender = Arc::new(Mutex::new(Some(tx)));
    let js = r#"(function(){
  try {
    var stores = [window.localStorage, window.sessionStorage];
    for (var s = 0; s < stores.length; s += 1) {
      var store = stores[s];
      if (!store) continue;
      for (var i = 0; i < store.length; i += 1) {
        var key = store.key(i);
        if (!key || key.indexOf("oidc.user:") !== 0) continue;
        var raw = store.getItem(key);
        if (!raw) continue;
        var parsed = JSON.parse(raw);
        if (!parsed || typeof parsed !== "object") continue;
        var token = parsed.access_token;
        var sub = parsed.profile && parsed.profile.sub;
        if (typeof token === "string" && token.length > 0 && typeof sub === "string" && sub.length > 0) {
          return JSON.stringify({
            storageKey: key,
            accessToken: token,
            refreshToken: typeof parsed.refresh_token === "string" ? parsed.refresh_token : null,
            expiresAt: typeof parsed.expires_at === "number" ? Math.floor(parsed.expires_at) : null,
            userId: sub
          });
        }
      }
    }
  } catch (e) {}
  return "";
})()"#;

    if window
        .eval_with_callback(js, move |val| {
            if let Some(tx) = sender.lock().unwrap_or_else(|e| e.into_inner()).take() {
                let _ = tx.send(val);
            }
        })
        .is_err()
    {
        return None;
    }

    let raw = tokio::time::timeout(Duration::from_secs(2), rx)
        .await
        .ok()?
        .ok()?;
    let unwrapped = unwrap_eval_string(&raw)?;
    if unwrapped.is_empty() {
        return None;
    }
    serde_json::from_str::<ExtractedOidc>(&unwrapped).ok()
}

// ─── URL / OIDC / JSON helpers ───────────────────────────────────────────────

pub fn is_allowed_main_frame_url(url: &Url) -> bool {
    super::is_allowed_main_frame_url(url, &["linxea.com"])
}

pub fn on_authenticated_page(url: &Url) -> bool {
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(url.port(), None | Some(443))
    {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    if !host.eq_ignore_ascii_case(APP_HOST) && !is_domain_or_subdomain(&host.to_ascii_lowercase(), "linxea.com") {
        return false;
    }
    if !host.eq_ignore_ascii_case(APP_HOST) {
        return false;
    }
    let path = url.path().to_ascii_lowercase();
    !path.starts_with("/authentification") && !path.contains("mfa-verification")
}

fn contract_id_from_url(url: &Url) -> Option<String> {
    let mut segments = url.path_segments()?;
    while let Some(seg) = segments.next() {
        if seg.eq_ignore_ascii_case("contrat") {
            let id = segments.next()?;
            if is_safe_path_segment(id) {
                return Some(id.to_owned());
            }
        }
    }
    None
}

fn is_safe_path_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn parse_oidc_key(key: &str) -> (Option<String>, Option<String>) {
    let Some(rest) = key.strip_prefix("oidc.user:") else {
        return (None, None);
    };
    let Some((authority, client_id)) = rest.rsplit_once(':') else {
        return (None, None);
    };
    if authority.starts_with("https://") && !client_id.is_empty() {
        (Some(authority.to_owned()), Some(client_id.to_owned()))
    } else {
        (None, None)
    }
}

fn token_currently_valid(expires_at: Option<u64>) -> bool {
    let Some(exp) = expires_at else {
        return true;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    exp > now.saturating_add(30)
}

fn parse_contracts_response(raw: &str) -> Result<Vec<ContractSummary>, ApiError> {
    let body: Value = serde_json::from_str(raw)
        .map_err(|_| ApiError::new(StatusCode::BAD_GATEWAY, "UPSTREAM_FORMAT_CHANGED"))?;
    let items = if let Some(arr) = body.as_array() {
        arr
    } else if let Some(arr) = body.get("items").and_then(Value::as_array) {
        arr
    } else {
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "UPSTREAM_FORMAT_CHANGED",
        ));
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Some(obj) = item.as_object() else {
            continue;
        };
        let contract_id = obj
            .get("contractId")
            .or_else(|| obj.get("id"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned();
        if contract_id.is_empty() {
            continue;
        }
        let is_active = obj
            .get("isActive")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let contract_type_code = obj
            .get("contractTypeCode")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let contract_number = obj
            .get("externalContractNumber")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let product_name = obj
            .get("productName")
            .or_else(|| obj.get("productKey"))
            .or_else(|| obj.get("externalContractNumber"))
            .and_then(Value::as_str)
            .unwrap_or("Linxea")
            .to_owned();
        out.push(ContractSummary {
            contract_id,
            contract_number,
            product_name,
            contract_type_code,
            is_active,
        });
    }
    Ok(out)
}

fn encode_session(envelope: &LinxeaSessionEnvelope) -> Result<String, ApiError> {
    let encoded = serde_json::to_string(envelope)
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR"))?;
    if encoded.len() > MAX_SESSION_STATE_BYTES {
        return Err(ApiError::new(StatusCode::BAD_GATEWAY, "INVALID_DATA"));
    }
    Ok(encoded)
}

fn decode_session_state(session_state: &str) -> Result<LinxeaSessionEnvelope, ApiError> {
    if session_state.is_empty() || session_state.len() > MAX_SESSION_STATE_BYTES {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "INVALID_DATA"));
    }
    serde_json::from_str::<LinxeaSessionEnvelope>(session_state)
        .map_err(|_| ApiError::new(StatusCode::UNAUTHORIZED, "SESSION_EXPIRED"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowed_main_frame_permits_linxea_https_only() {
        assert!(is_allowed_main_frame_url(
            &Url::parse("https://espaceclient.linxea.com/authentification").unwrap()
        ));
        assert!(is_allowed_main_frame_url(
            &Url::parse("https://auth.linxea.com/oauth2/authorize").unwrap()
        ));
        assert!(!is_allowed_main_frame_url(
            &Url::parse("http://espaceclient.linxea.com/authentification").unwrap()
        ));
        assert!(!is_allowed_main_frame_url(
            &Url::parse("https://evil-linxea.com/").unwrap()
        ));
    }

    #[test]
    fn authenticated_page_detects_contract_and_savings_routes() {
        assert!(!on_authenticated_page(
            &Url::parse("https://espaceclient.linxea.com/authentification").unwrap()
        ));
        assert!(!on_authenticated_page(
            &Url::parse("https://espaceclient.linxea.com/authentification/callback").unwrap()
        ));
        assert!(!on_authenticated_page(
            &Url::parse("https://espaceclient.linxea.com/mfa-verification").unwrap()
        ));
        assert!(on_authenticated_page(
            &Url::parse("https://espaceclient.linxea.com/epargne").unwrap()
        ));
        assert!(on_authenticated_page(
            &Url::parse(
                "https://espaceclient.linxea.com/epargne/contrat/00000000-0000-4000-8000-000000000001"
            )
            .unwrap()
        ));
    }

    #[test]
    fn extracts_contract_uuid_and_oidc_key_parts() {
        let url = Url::parse(
            "https://espaceclient.linxea.com/epargne/contrat/00000000-0000-4000-8000-000000000001",
        )
        .unwrap();
        assert_eq!(
            contract_id_from_url(&url).as_deref(),
            Some("00000000-0000-4000-8000-000000000001")
        );
        assert_eq!(
            parse_oidc_key("oidc.user:https://auth.linxea.com:espace-client"),
            (
                Some("https://auth.linxea.com".to_owned()),
                Some("espace-client".to_owned())
            )
        );
    }

    #[test]
    fn parses_contracts_payload_and_roundtrips_session_envelope() {
        let contracts = parse_contracts_response(
            r#"{"items":[{"contractId":"c-1","externalContractNumber":"123456","productName":"Linxea Spirit PER","contractTypeCode":"PER","isActive":true}]}"#,
        )
        .unwrap();
        assert_eq!(contracts.len(), 1);
        assert_eq!(contracts[0].contract_id, "c-1");
        assert_eq!(contracts[0].contract_type_code, "PER");

        let env = LinxeaSessionEnvelope {
            user_id: "u-1".to_owned(),
            access_token: "tok-1".to_owned(),
            refresh_token: Some("ref-1".to_owned()),
            expires_at: Some(4_000_000_000),
            authority: Some("https://auth.linxea.com".to_owned()),
            client_id: Some("spa".to_owned()),
            fallback_contract_ids: vec!["c-1".to_owned()],
            desktop_only: Some(true),
            nonce: Some("abc".to_owned()),
        };
        let encoded = encode_session(&env).unwrap();
        let decoded = decode_session_state(&encoded).unwrap();
        assert_eq!(decoded.user_id, "u-1");
        assert_eq!(decoded.fallback_contract_ids, vec!["c-1".to_owned()]);
    }
}
