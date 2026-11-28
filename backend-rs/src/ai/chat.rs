//! Integrated AI Assistant chat sessions grounded in the signed-in member's live portfolio.
//! Reuses Picsou's runtime AI provider configuration (Ollama, LM Studio, OpenAI, Anthropic, Gemini).

use axum::extract::{Path as AxumPath, State};
use chrono::{DateTime, Utc};
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

use super::{DynamicCategorizer, is_configured, provider};

const ASSISTANT_SYSTEM_PROMPT: &str = "\
You are Picsou Assistant, a concise, accurate, privacy-respecting personal finance analyst. \
Answer strictly from the provided portfolio snapshot and conversation history. \
Never invent account balances, tickers, or tax rules. When giving numbers, format them clearly in EUR. \
Reply in the same language as the user's message.";

pub fn register(routes: &mut Routes) {
    routes
        .get("/api/ai/chat/sessions", list_sessions)
        .post("/api/ai/chat/sessions", create_session)
        .get("/api/ai/chat/sessions/{id}", get_session)
        .delete("/api/ai/chat/sessions/{id}", delete_session)
        .post("/api/ai/chat/sessions/{id}/messages", post_message);
}

#[derive(Debug, Clone, Serialize, DbFromRow)]
#[serde(rename_all = "camelCase")]
pub struct ChatSession {
    pub id: i64,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, DbFromRow)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub id: i64,
    pub role: String,
    pub content: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatSessionDetail {
    session: ChatSession,
    messages: Vec<ChatMessage>,
}

async fn list_sessions(
    State(state): State<AppState>,
    auth: Auth,
) -> AppResult<axum::Json<Vec<ChatSession>>> {
    let rows: Vec<ChatSession> = crate::db::query_as(
        "SELECT id, title, created_at, updated_at FROM ai_chat_session \
         WHERE member_id = $1 ORDER BY updated_at DESC, id DESC LIMIT 50",
    )
    .bind(auth.member_id)
    .fetch_all(&state.db)
    .await?;
    Ok(axum::Json(rows))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSessionRequest {
    title: Option<String>,
}

impl Validate for CreateSessionRequest {
    fn validate(&self, _: &mut Errors) {}
}

async fn create_session(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<CreateSessionRequest>,
) -> AppResult<(StatusCode, axum::Json<ChatSession>)> {
    let now = state.clock.now();
    let title = req
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Portfolio chat")
        .chars()
        .take(160)
        .collect::<String>();
    let row: ChatSession = crate::db::query_as(
        "INSERT INTO ai_chat_session (member_id, title, created_at, updated_at) \
         VALUES ($1, $2, $3, $3) RETURNING id, title, created_at, updated_at",
    )
    .bind(auth.member_id)
    .bind(&title)
    .bind(now)
    .fetch_one(&state.db)
    .await?;
    Ok((StatusCode::CREATED, axum::Json(row)))
}

async fn load_session_detail(
    state: &AppState,
    member_id: i64,
    session_id: i64,
) -> AppResult<ChatSessionDetail> {
    let session: ChatSession = crate::db::query_as(
        "SELECT id, title, created_at, updated_at FROM ai_chat_session \
         WHERE id = $1 AND member_id = $2",
    )
    .bind(session_id)
    .bind(member_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Chat session not found".to_owned()))?;

    let messages: Vec<ChatMessage> = crate::db::query_as(
        "SELECT id, role, content, provider, model, created_at FROM ai_chat_message \
         WHERE session_id = $1 ORDER BY id ASC",
    )
    .bind(session_id)
    .fetch_all(&state.db)
    .await?;

    Ok(ChatSessionDetail { session, messages })
}

async fn get_session(
    State(state): State<AppState>,
    auth: Auth,
    AxumPath(id): AxumPath<i64>,
) -> AppResult<axum::Json<ChatSessionDetail>> {
    Ok(axum::Json(load_session_detail(&state, auth.member_id, id).await?))
}

async fn delete_session(
    State(state): State<AppState>,
    auth: Auth,
    AxumPath(id): AxumPath<i64>,
) -> AppResult<StatusCode> {
    let affected = crate::db::query(
        "DELETE FROM ai_chat_session WHERE id = $1 AND member_id = $2",
    )
    .bind(id)
    .bind(auth.member_id)
    .execute(&state.db)
    .await?
    .rows_affected();
    if affected == 0 {
        return Err(AppError::NotFound("Chat session not found".to_owned()));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PostMessageRequest {
    content: String,
}

impl Validate for PostMessageRequest {
    fn validate(&self, errors: &mut Errors) {
        let c = self.content.trim();
        if c.is_empty() || c.chars().count() > 4000 {
            errors.add("content", "Message must be between 1 and 4000 characters");
        }
    }
}

#[derive(DbFromRow)]
struct AccountSummaryRow {
    name: String,
    account_type: String,
    currency: String,
    current_balance: Decimal,
}

#[derive(DbFromRow)]
struct HoldingSummaryRow {
    ticker: String,
    name: Option<String>,
    quantity: Decimal,
    current_price: Option<Decimal>,
    provider_value_eur: Option<Decimal>,
}

async fn build_portfolio_context(state: &AppState, member_id: i64) -> AppResult<String> {
    let accounts: Vec<AccountSummaryRow> = crate::db::query_as(
        "SELECT name, type::text AS account_type, currency, current_balance \
         FROM account WHERE member_id = $1 AND deleted_at IS NULL AND hidden = false \
         ORDER BY current_balance DESC LIMIT 40",
    )
    .bind(member_id)
    .fetch_all(&state.db)
    .await?;

    let holdings: Vec<HoldingSummaryRow> = crate::db::query_as(
        "SELECT h.ticker, h.name, h.quantity, h.current_price, h.provider_value_eur \
         FROM account_holding h \
         JOIN account a ON a.id = h.account_id \
         WHERE a.member_id = $1 AND a.deleted_at IS NULL AND a.hidden = false \
         ORDER BY COALESCE(h.provider_value_eur, h.quantity) DESC LIMIT 25",
    )
    .bind(member_id)
    .fetch_all(&state.db)
    .await?;

    let mut total_assets = 0.0_f64;
    let mut total_liabilities = 0.0_f64;
    let mut lines = Vec::new();
    for a in &accounts {
        let bal = a.current_balance.to_f64().unwrap_or(0.0);
        if a.account_type == "LOAN" {
            total_liabilities += bal.abs();
        } else {
            total_assets += bal;
        }
        lines.push(format!(
            "- {} ({}, {}): {:.2}",
            a.name, a.account_type, a.currency, bal
        ));
    }

    let mut holding_lines = Vec::new();
    for h in &holdings {
        let val = h
            .provider_value_eur
            .and_then(|d| d.to_f64())
            .or_else(|| h.current_price.and_then(|p| (p * h.quantity).to_f64()))
            .unwrap_or(0.0);
        holding_lines.push(format!(
            "- {} ({}): qty={}, est_eur={:.2}",
            h.ticker,
            h.name.as_deref().unwrap_or(""),
            h.quantity,
            val
        ));
    }

    Ok(format!(
        "Portfolio snapshot (date {}):\nTotal assets: {:.2} EUR\nTotal liabilities: {:.2} EUR\nNet worth: {:.2} EUR\n\nAccounts:\n{}\n\nTop holdings:\n{}",
        state.clock.today(),
        total_assets,
        total_liabilities,
        total_assets - total_liabilities,
        lines.join("\n"),
        holding_lines.join("\n"),
    ))
}

async fn post_message(
    State(state): State<AppState>,
    auth: Auth,
    AxumPath(session_id): AxumPath<i64>,
    Json(req): Json<PostMessageRequest>,
) -> AppResult<axum::Json<ChatSessionDetail>> {
    let detail = load_session_detail(&state, auth.member_id, session_id).await?;
    let now = state.clock.now();
    let user_text = req.content.trim().to_owned();

    crate::db::query(
        "INSERT INTO ai_chat_message (session_id, role, content, created_at) \
         VALUES ($1, 'USER', $2, $3)",
    )
    .bind(session_id)
    .bind(&user_text)
    .bind(now)
    .execute(&state.db)
    .await?;

    let context = build_portfolio_context(&state, auth.member_id).await?;
    let history_snippet: String = detail
        .messages
        .iter()
        .rev()
        .take(8)
        .rev()
        .map(|m| format!("{}: {}", m.role, m.content))
        .collect::<Vec<_>>()
        .join("\n");

    let full_prompt = format!(
        "{context}\n\nRecent conversation:\n{history_snippet}\n\nUser question:\n{user_text}"
    );

    let categorizer = DynamicCategorizer::new(
        state.db.clone(),
        state.crypto.clone(),
        state.http.clone(),
    );

    let (reply_text, used_provider, used_model) = match categorizer.current_config().await {
        Ok(Some(cfg)) if is_configured(&cfg) => {
            let http = provider::build_http_client();
            match provider::call_provider(
                &http,
                &cfg,
                ASSISTANT_SYSTEM_PROMPT,
                &full_prompt,
                provider::DEFAULT_TIMEOUT,
            )
            .await
            {
                Ok(resp) => (
                    resp.text
                        .unwrap_or_else(|| "The model returned an empty answer.".to_owned()),
                    Some(cfg.provider.name().to_owned()),
                    Some(cfg.effective_model()),
                ),
                Err(err) => (
                    format!("AI provider error: {err}"),
                    Some(cfg.provider.name().to_owned()),
                    Some(cfg.effective_model()),
                ),
            }
        }
        _ => (
            format!(
                "No AI provider is currently configured in Administration > AI. \
                 Configure Ollama/LM Studio or an API key to unlock natural-language analysis.\n\n\
                 Here is your live portfolio summary:\n{context}"
            ),
            Some("local-summary".to_owned()),
            None,
        ),
    };

    let reply_at = state.clock.now();
    crate::db::query(
        "INSERT INTO ai_chat_message (session_id, role, content, provider, model, created_at) \
         VALUES ($1, 'ASSISTANT', $2, $3, $4, $5)",
    )
    .bind(session_id)
    .bind(&reply_text)
    .bind(used_provider.as_deref())
    .bind(used_model.as_deref())
    .bind(reply_at)
    .execute(&state.db)
    .await?;

    crate::db::query("UPDATE ai_chat_session SET updated_at = $1 WHERE id = $2")
        .bind(reply_at)
        .bind(session_id)
        .execute(&state.db)
        .await?;

    Ok(axum::Json(
        load_session_detail(&state, auth.member_id, session_id).await?,
    ))
}
