//! End-to-end HTTP + SQLite integration tests covering every roadmap endpoint on a real
//! migrated embedded SQLite database (`V1..V7`, schema-verified).

use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::body::Body;
use chrono::{TimeZone, Utc};
use http::{Method, Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::AppState;
use crate::clock::FixedClock;
use crate::config::{Config, Env};
use crate::db::Db;
use crate::db::sqlite::SqliteDb;

async fn setup_app() -> (axum::Router, String, i64, i64, i64) {
    let sq = SqliteDb::open_in_memory().expect("open sqlite memory");
    sq.migrate().expect("apply V1..V7 migrations");
    sq.ensure_seeded().expect("seed sqlite");
    sq.verify_schema().expect("schema.tsv parity");

    let db = Db::Sqlite(sq);
    let now = Utc.with_ymd_and_hms(2026, 6, 15, 10, 0, 0).unwrap();
    let clock = Arc::new(FixedClock::new(now));
    let env = Env::from_map([
        ("DATABASE_URL", "sqlite::memory:"),
        ("JWT_SECRET", "test-jwt-secret-must-be-at-least-32-bytes-long!!"),
        ("CRYPTO_ENCRYPTION_KEY", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="),
        ("PICSOU_OS_KEYSTORE", "off"),
    ]);
    let mut config = Config::load(&env).expect("test config");
    config.desktop_mode = false;

    let state = AppState::new(config, db.clone(), clock).expect("app state");
    state.setup_complete.store(true, Ordering::SeqCst);

    // Seed one family member, user, account, category, transaction, holding, and snapshot.
    let member_id: i64 = crate::db::query_scalar(
        "INSERT INTO family_member (display_name, is_managed) VALUES ('Alex', false) RETURNING id",
    )
    .fetch_one(&db)
    .await
    .unwrap();

    let user_id: i64 = crate::db::query_scalar(
        "INSERT INTO app_user (username, password_hash, role, member_id, is_activated, token_version) \
         VALUES ('alex', '$2b$04$xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx', 'ADMIN', $1, true, 1) \
         RETURNING id",
    )
    .bind(member_id)
    .fetch_one(&db)
    .await
    .unwrap();

    let account_id: i64 = crate::db::query_scalar(
        "INSERT INTO account (name, type, currency, current_balance, is_manual, member_id, hidden) \
         VALUES ('PEA Test', 'PEA'::account_type, 'EUR', 25000.00, true, $1, false) RETURNING id",
    )
    .bind(member_id)
    .fetch_one(&db)
    .await
    .unwrap();

    crate::db::query(
        "INSERT INTO account_holding (account_id, ticker, name, quantity, current_price, average_buy_in, provider_value_eur) \
         VALUES ($1, 'CW8.PA', 'Amundi MSCI World', 50, 500, 420, 25000)",
    )
    .bind(account_id)
    .execute(&db)
    .await
    .unwrap();

    crate::db::query(
        "INSERT INTO balance_snapshot (account_id, date, balance, invested_amount) \
         VALUES ($1, '2026-01-15', 20000, 18000), ($1, '2026-06-15', 25000, 21000)",
    )
    .bind(account_id)
    .execute(&db)
    .await
    .unwrap();

    let cat_a: i64 = crate::db::query_scalar(
        "INSERT INTO category (member_id, name, kind, icon, color, is_default, archived) \
         VALUES ($1, 'Courses', 'EXPENSE'::category_kind, 'bag', '#3b82f6', false, false) RETURNING id",
    )
    .bind(member_id)
    .fetch_one(&db)
    .await
    .unwrap();

    let cat_b: i64 = crate::db::query_scalar(
        "INSERT INTO category (member_id, name, kind, icon, color, is_default, archived) \
         VALUES ($1, 'Maison', 'EXPENSE'::category_kind, 'home', '#10b981', false, false) RETURNING id",
    )
    .bind(member_id)
    .fetch_one(&db)
    .await
    .unwrap();

    let tx_id: i64 = crate::db::query_scalar(
        "INSERT INTO transaction (account_id, date, description, amount, native_currency, is_manual, budget_neutral, category_manual) \
         VALUES ($1, '2026-06-10', 'Hypermarche', -100.00, 'EUR', true, false, false) RETURNING id",
    )
    .bind(account_id)
    .fetch_one(&db)
    .await
    .unwrap();

    // Also add one dividend transaction for income summary
    crate::db::query(
        "INSERT INTO transaction (account_id, date, description, amount, native_currency, is_manual, budget_neutral, category_manual, tx_type, ticker, name) \
         VALUES ($1, '2026-05-10', 'Dividende Air Liquide', 120.50, 'EUR', true, true, false, 'DIVIDEND', 'AI.PA', 'Air Liquide')",
    )
    .bind(account_id)
    .execute(&db)
    .await
    .unwrap();

    let user = crate::auth::users::find_by_id(&db, user_id)
        .await
        .expect("find seeded user")
        .expect("seeded user exists");
    let token = state.jwt.access_token(&user, None);
    let cookie = format!("access_token={token}");
    let app = crate::app(state);
    (app, cookie, account_id, tx_id, (cat_a << 32) | cat_b)
}

async fn req_json(
    app: &axum::Router,
    method: Method,
    uri: &str,
    cookie: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, cookie);
    let body = if let Some(json_body) = body {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
        Body::from(serde_json::to_vec(&json_body).unwrap())
    } else {
        Body::empty()
    };
    let resp = app.clone().oneshot(builder.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .unwrap()
        .to_bytes();
    let val = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, val)
}

#[tokio::test]
async fn all_roadmap_endpoints_execute_cleanly_on_sqlite() {
    let (app, cookie, account_id, tx_id, cats) = setup_app().await;
    let cat_a = cats >> 32;
    let cat_b = cats & 0xFFFF_FFFF;

    // 1. Health Center + dismiss / restore
    let (st, health) = req_json(&app, Method::GET, "/api/health-center", &cookie, None).await;
    assert_eq!(st, StatusCode::OK, "health: {health}");
    assert!(health["summary"]["score"].as_u64().is_some());

    let (st, _) = req_json(
        &app,
        Method::POST,
        "/api/health-center/dismiss",
        &cookie,
        Some(json!({ "issueKey": "unclassified_holdings" })),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);

    // 2. Contribution Limits + presets
    let (st, presets) = req_json(&app, Method::GET, "/api/contribution-limits/presets", &cookie, None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(presets.as_array().unwrap().len() >= 5);

    let (st, created) = req_json(
        &app,
        Method::POST,
        "/api/contribution-limits",
        &cookie,
        Some(json!({
            "name": "PEA",
            "envelopeKind": "PEA",
            "limitAmount": 150000,
            "periodKind": "LIFETIME",
            "currency": "EUR",
            "accountIds": [account_id]
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "limit create: {created}");

    let (st, limits) = req_json(&app, Method::GET, "/api/contribution-limits", &cookie, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(limits.as_array().unwrap().len(), 1);
    assert_eq!(limits[0]["contributedAmount"], json!(25000.0));

    // 3. Rebalancing Worksheet
    let (st, rebal) = req_json(
        &app,
        Method::POST,
        "/api/analysis/rebalance-plan",
        &cookie,
        Some(json!({ "cashContributionEur": 5000, "driftBandPct": 5, "cashOnly": true })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "rebalance: {rebal}");
    assert_eq!(rebal["rows"].as_array().unwrap().len(), 5);

    // 4. Performance TWR/MWR + Benchmarks
    let (st, perf) = req_json(&app, Method::GET, "/api/wealth/performance?range=1Y", &cookie, None).await;
    assert_eq!(st, StatusCode::OK, "perf: {perf}");
    assert_eq!(perf["series"].as_array().unwrap().len(), 2);

    // 5. FIRE / Retirement & Save-up Simulators
    let (st, fire) = req_json(
        &app,
        Method::POST,
        "/api/wealth/planning/retirement",
        &cookie,
        Some(json!({
            "currentAge": 35,
            "retirementAge": 60,
            "lifeExpectancy": 90,
            "currentSavingsEur": 80000,
            "monthlyContributionEur": 1200,
            "expectedAnnualReturnPct": 6.5,
            "inflationPct": 2.0,
            "monthlyRetirementSpendEur": 2500,
            "safeWithdrawalRatePct": 4.0
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "fire: {fire}");
    assert_eq!(fire["fiNumberEur"], json!(750000.0));

    // 6. Virtual Portfolios
    let (st, vp) = req_json(
        &app,
        Method::POST,
        "/api/portfolios",
        &cookie,
        Some(json!({ "name": "Actions Long Terme", "accountIds": [account_id] })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "vp: {vp}");
    let (st, vps) = req_json(&app, Method::GET, "/api/portfolios", &cookie, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(vps[0]["totalValueEur"], json!(25000.0));

    // 7. Custom Taxonomies
    let (st, tax) = req_json(
        &app,
        Method::POST,
        "/api/taxonomies",
        &cookie,
        Some(json!({ "name": "Liquidite" })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let tax_id = tax["id"].as_i64().unwrap();
    let (st, cat) = req_json(
        &app,
        Method::POST,
        &format!("/api/taxonomies/{tax_id}/categories"),
        &cookie,
        Some(json!({ "name": "ETF Liquides", "targetPercent": 80 })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let cat_id = cat["id"].as_i64().unwrap();
    let (st, _) = req_json(
        &app,
        Method::POST,
        &format!("/api/taxonomies/{tax_id}/assignments"),
        &cookie,
        Some(json!({ "categoryId": cat_id, "ticker": "CW8.PA", "weightPercent": 100 })),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, taxes) = req_json(&app, Method::GET, "/api/taxonomies", &cookie, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(taxes[0]["totalClassifiedEur"], json!(25000.0));

    // 8. Consolidated Income
    let (st, inc) = req_json(&app, Method::GET, "/api/wealth/income?year=2026", &cookie, None).await;
    assert_eq!(st, StatusCode::OK, "income: {inc}");
    assert_eq!(inc["totalEur"], json!(120.5));

    // 9. Budget Transaction Splits & Spending Events
    let (st, _) = req_json(
        &app,
        Method::PUT,
        &format!("/api/budget/transactions/{tx_id}/splits"),
        &cookie,
        Some(json!({
            "splits": [
                { "categoryId": cat_a, "amount": -60.0 },
                { "categoryId": cat_b, "amount": -40.0 }
            ]
        })),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, splits) = req_json(
        &app,
        Method::GET,
        &format!("/api/budget/transactions/{tx_id}/splits"),
        &cookie,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(splits.as_array().unwrap().len(), 2);

    let (st, ev) = req_json(
        &app,
        Method::POST,
        "/api/budget/events",
        &cookie,
        Some(json!({ "name": "Vacances 2026", "budgetAmount": 1500 })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let ev_id = ev["id"].as_i64().unwrap();
    let (st, _) = req_json(
        &app,
        Method::POST,
        &format!("/api/budget/events/{ev_id}/transactions/{tx_id}"),
        &cookie,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, events) = req_json(&app, Method::GET, "/api/budget/events", &cookie, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(events[0]["spentAmount"], json!(100.0));

    // 10. Cost Basis Method + Stock Splits
    let (st, _) = req_json(
        &app,
        Method::PATCH,
        &format!("/api/accounts/{account_id}/cost-basis-method"),
        &cookie,
        Some(json!({ "method": "FIFO" })),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, sp) = req_json(
        &app,
        Method::POST,
        "/api/stock-splits",
        &cookie,
        Some(json!({ "ticker": "CW8.PA", "splitDate": "2026-03-01", "ratioNumerator": 2, "ratioDenominator": 1 })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "split: {sp}");
    let (st, rpnl) = req_json(
        &app,
        Method::GET,
        &format!("/api/accounts/{account_id}/realized-pnl"),
        &cookie,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "rpnl: {rpnl}");
    assert_eq!(rpnl["method"], json!("FIFO"));

    // 11. AI Chat + AI CSV Mapper + Offline FinanceDatabase Search + Storage Security
    let (st, sess) = req_json(
        &app,
        Method::POST,
        "/api/ai/chat/sessions",
        &cookie,
        Some(json!({ "title": "Bilan" })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let sid = sess["id"].as_i64().unwrap();
    let (st, detail) = req_json(
        &app,
        Method::POST,
        &format!("/api/ai/chat/sessions/{sid}/messages"),
        &cookie,
        Some(json!({ "content": "Resume mon patrimoine" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "ai chat: {detail}");
    assert_eq!(detail["messages"].as_array().unwrap().len(), 2);

    let (st, csv_map) = req_json(
        &app,
        Method::POST,
        "/api/ai/csv/suggest-mapping",
        &cookie,
        Some(json!({ "mode": "CASH", "headers": ["Date", "Libelle", "Montant"] })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(csv_map["suggestedMapping"]["amount"], json!(2));

    let (st, catalog) = req_json(&app, Method::GET, "/api/market/search?q=LU1681043599", &cookie, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(catalog[0]["ticker"], json!("CW8.PA"));

    let (st, sec) = req_json(&app, Method::GET, "/api/backup/storage-security", &cookie, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(sec["engine"], json!("sqlite"));
}
