//! LinxeaController and LinxeaSyncService over HTTP, the sidecar stubbed.

use picsou::ledger::sidecar::SidecarBank;
use serde_json::{Value, json};
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::support::{admin, app_with_sidecar, d, member, session_of, settle, store_session};
use rust_decimal::Decimal;

const URL: &str = "LINXEA_AUTH_URL";
const BANK: SidecarBank = SidecarBank::Linxea;

fn n(number: &str) -> Value {
    serde_json::from_str(number).expect("JSON number")
}

fn position(
    isin: Option<&str>,
    symbol: &str,
    quantity: &str,
    value: &str,
    pnl: &str,
) -> Value {
    json!({
        "isin": isin, "symbol": symbol, "label": format!("{symbol} support"),
        "quantity": n(quantity), "buyingPriceEur": 80, "currentPrice": 100,
        "quoteCurrency": "EUR", "currentValueEur": n(value), "pnlEur": n(pnl)
    })
}

fn account(account_type: &str, positions: Vec<Value>, balance: &str) -> Value {
    json!({
        "externalId": "linxea_per-1", "name": "Linxea Spirit PER (123456789)",
        "type": account_type, "balanceEur": n(balance), "cashBalance": 0,
        "positions": positions, "snapshotComplete": true
    })
}

async fn mount_accounts(sidecar: &MockServer, body: Value) {
    Mock::given(method("POST"))
        .and(path("/accounts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(sidecar)
        .await;
}

async fn status(client: &picsou::testing::Client) -> Value {
    client.get("/api/linxea/status").send().await.json()
}

#[tokio::test]
async fn status_without_a_session_is_inactive_and_idle() {
    let sidecar = MockServer::start().await;
    let app = app_with_sidecar(URL, &sidecar).await;
    let (client, _) = admin(&app).await;

    let response = client.get("/api/linxea/status").send().await;

    assert_eq!(response.status, 200);
    assert_eq!(
        response.json(),
        json!({ "isActive": false, "syncStatus": "IDLE" })
    );
}

#[tokio::test]
async fn initiate_relays_the_emailed_code_challenge_and_stores_nothing() {
    let sidecar = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/initiate"))
        .and(body_json(json!({ "login": "alice@example.com", "password": "secret" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "processId": "p-1", "mfaRequired": true, "mfaType": "EMAIL_CODE"
        })))
        .expect(1)
        .mount(&sidecar)
        .await;
    let app = app_with_sidecar(URL, &sidecar).await;
    let (client, member_id) = admin(&app).await;

    let response = client
        .post("/api/linxea/auth/initiate")
        .json(&json!({ "login": "alice@example.com", "password": "secret" }))
        .send()
        .await;

    assert_eq!(response.status, 200);
    assert_eq!(
        response.json(),
        json!({ "processId": "p-1", "mfaRequired": true, "mfaType": "EMAIL_CODE" })
    );
    assert!(session_of(&app, BANK, member_id).await.is_none());
}

#[tokio::test]
async fn complete_writes_the_contract_and_its_supports() {
    let sidecar = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/complete"))
        .and(body_json(json!({ "processId": "p-1", "code": "123456" })))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "sessionState": "plain-state" })),
        )
        .mount(&sidecar)
        .await;
    // The fonds euro arrives as one unit at its value, not zero parts.
    let mut euro = position(None, "LINXEA_EURO_FUND", "1", "250.00", "0");
    euro["buyingPriceEur"] = json!(250.00);
    euro["currentPrice"] = json!(250.00);
    mount_accounts(
        &sidecar,
        json!([account(
            "PER",
            vec![
                euro,
                position(Some("IE0000000001"), "IE0000000001", "10.5", "1260.00", "210.00"),
            ],
            "1510.00"
        )]),
    )
    .await;
    let app = app_with_sidecar(URL, &sidecar).await;
    let (client, member_id) = admin(&app).await;

    let response = client
        .post("/api/linxea/auth/complete")
        .json(&json!({ "processId": "p-1", "code": "123456" }))
        .send()
        .await;
    settle(&app).await;

    assert_eq!(response.status, 200);
    assert_eq!(response.json()["isActive"], true);
    assert_eq!(status(&client).await["syncStatus"], "SUCCESS");
    let (name, kind, provider, manual, balance, color): (
        String,
        String,
        String,
        bool,
        Decimal,
        String,
    ) = picsou::db::query_as(
        "SELECT name, type::text, provider, is_manual, current_balance, color \
         FROM account WHERE external_account_id = 'linxea_per-1' AND member_id = $1",
    )
    .bind(member_id)
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    assert_eq!(
        (
            name.as_str(),
            kind.as_str(),
            provider.as_str(),
            manual,
            color.as_str()
        ),
        (
            "Linxea Spirit PER (123456789)",
            "PER",
            "Linxea",
            false,
            "#f59e0b"
        )
    );
    assert_eq!(balance, d("1510.00"));
    let rows: Vec<(String, Decimal, Decimal, Decimal)> = picsou::db::query_as(
        "SELECT ticker, quantity, provider_value_eur, provider_pnl_eur \
         FROM account_holding ORDER BY ticker",
    )
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            ("IE0000000001".into(), d("10.5"), d("1260.00"), d("210.00")),
            ("LINXEA_EURO_FUND".into(), d("1"), d("250.00"), d("0")),
        ]
    );
    let snapshot: (Decimal, Decimal) =
        picsou::db::query_as("SELECT balance, invested_amount FROM balance_snapshot")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(
        snapshot,
        (d("1510.00"), d("1090.00")),
        "10.5 x 80 plus the euro fund at its value"
    );
}

#[tokio::test]
async fn a_rejected_code_maps_to_invalid_otp_with_the_sidecars_wording() {
    let sidecar = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/complete"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&sidecar)
        .await;
    let app = app_with_sidecar(URL, &sidecar).await;
    let (client, member_id) = admin(&app).await;

    let response = client
        .post("/api/linxea/auth/complete")
        .json(&json!({ "processId": "p-1", "code": "123456" }))
        .send()
        .await;

    assert_eq!(response.status, 422);
    let body = response.json();
    assert_eq!(body["code"], "INVALID_OTP");
    assert_eq!(
        body["detail"],
        SidecarBank::Linxea
            .message(picsou::ledger::sidecar::SidecarErrorCode::InvalidOtp)
            .unwrap()
    );
    assert!(session_of(&app, BANK, member_id).await.is_none());
}

#[tokio::test]
async fn payload_validation_rejects_a_malformed_login_and_code() {
    let sidecar = MockServer::start().await;
    let app = app_with_sidecar(URL, &sidecar).await;
    let (client, _) = admin(&app).await;

    let initiate = client
        .post("/api/linxea/auth/initiate")
        .json(&json!({ "login": "not an email@@", "password": "secret" }))
        .send()
        .await;
    let complete = client
        .post("/api/linxea/auth/complete")
        .json(&json!({ "processId": "p-1", "code": "12a456" }))
        .send()
        .await;

    assert_eq!(initiate.status, 422);
    assert_eq!(
        initiate.json()["errors"]["login"],
        r#"must match "[^\s@]+@[^\s@]+\.[^\s@]+""#
    );
    assert_eq!(complete.status, 422);
    assert_eq!(complete.json()["errors"]["code"], r#"must match "\d{6}""#);
    assert!(sidecar.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn the_sixth_authentication_attempt_from_one_ip_is_rate_limited() {
    let sidecar = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/initiate"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&sidecar)
        .await;
    let app = app_with_sidecar(URL, &sidecar).await;
    let (client, _) = admin(&app).await;
    let attempt = || {
        client
            .post("/api/linxea/auth/initiate")
            .json(&json!({ "login": "alice@example.com", "password": "wrong" }))
            .send()
    };

    for _ in 0..5 {
        let response = attempt().await;
        assert_eq!(response.status, 422);
        assert_eq!(response.json()["code"], "INVALID_CREDENTIALS");
    }
    let limited = attempt().await;

    assert_eq!(limited.status, 429);
    assert_eq!(
        limited.json()["detail"],
        "Too many Linxea authentication attempts. Please wait before retrying."
    );
    assert_eq!(sidecar.received_requests().await.unwrap().len(), 5);
}

#[tokio::test]
async fn sync_without_a_session_asks_to_reconnect() {
    let sidecar = MockServer::start().await;
    let app = app_with_sidecar(URL, &sidecar).await;
    let (client, _) = admin(&app).await;

    let response = client.post("/api/linxea/sync").send().await;

    assert_eq!(response.status, 422);
    let body = response.json();
    assert_eq!(body["code"], "SESSION_EXPIRED");
    assert_eq!(
        body["detail"],
        "No active Linxea session. Please reconnect."
    );
}

#[tokio::test]
async fn an_expired_session_is_deactivated_and_stays_observable() {
    let sidecar = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/accounts"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({ "detail": "SESSION_EXPIRED" })),
        )
        .mount(&sidecar)
        .await;
    let app = app_with_sidecar(URL, &sidecar).await;
    let (client, member_id) = admin(&app).await;
    store_session(&app, BANK, member_id, "plain-state").await;

    client.post("/api/linxea/sync").send().await;
    settle(&app).await;

    let after = status(&client).await;
    assert_eq!(after["isActive"], false);
    assert_eq!(after["syncStatus"], "FAILED");
    assert_eq!(after["lastSyncError"], "SESSION_EXPIRED");
    let again = client.post("/api/linxea/sync").send().await;
    assert_eq!(again.status, 422);
    assert_eq!(
        again.json()["detail"],
        "The Linxea session expired. Please reconnect."
    );
}

#[tokio::test]
async fn an_unsupported_contract_type_is_refused_whole() {
    let sidecar = MockServer::start().await;
    mount_accounts(&sidecar, json!([account("CHECKING", vec![], "0")])).await;
    let app = app_with_sidecar(URL, &sidecar).await;
    let (client, member_id) = admin(&app).await;
    store_session(&app, BANK, member_id, "plain-state").await;

    client.post("/api/linxea/sync").send().await;
    settle(&app).await;

    assert_eq!(status(&client).await["lastSyncError"], "INVALID_DATA");
    let accounts: i64 = picsou::db::query_scalar("SELECT count(*) FROM account")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(accounts, 0);
}

#[tokio::test]
async fn a_soft_deleted_contract_is_not_resurrected() {
    let sidecar = MockServer::start().await;
    mount_accounts(&sidecar, json!([account("PER", vec![], "0")])).await;
    let app = app_with_sidecar(URL, &sidecar).await;
    let (client, member_id) = admin(&app).await;
    picsou::db::query(
        "INSERT INTO account (member_id, name, type, currency, current_balance, external_account_id, is_manual, \
         color, deleted_at) VALUES ($1, 'Gone', 'PER', 'EUR', 0, 'linxea_per-1', false, '#fff', now())",
    )
    .bind(member_id)
    .execute(&app.state.db)
    .await
    .unwrap();
    store_session(&app, BANK, member_id, "plain-state").await;

    client.post("/api/linxea/sync").send().await;
    settle(&app).await;

    assert_eq!(status(&client).await["syncStatus"], "SUCCESS");
    let live: i64 =
        picsou::db::query_scalar("SELECT count(*) FROM account WHERE deleted_at IS NULL")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(live, 0);
}

#[tokio::test]
async fn status_and_clear_use_only_the_current_member() {
    let sidecar = MockServer::start().await;
    let app = app_with_sidecar(URL, &sidecar).await;
    let (admin_client, admin_id) = admin(&app).await;
    let (other, other_id) = member(&app, "bob").await;
    store_session(&app, BANK, admin_id, "admin-state").await;

    assert_eq!(status(&other).await["isActive"], false);
    let cleared = other.delete("/api/linxea/session").send().await;

    assert_eq!(cleared.status, 204);
    assert!(session_of(&app, BANK, admin_id).await.is_some());
    assert!(session_of(&app, BANK, other_id).await.is_none());
    assert_eq!(status(&admin_client).await["isActive"], true);
}

#[tokio::test]
async fn startup_recovery_fails_in_flight_syncs() {
    let sidecar = MockServer::start().await;
    let app = app_with_sidecar(URL, &sidecar).await;
    let (client, member_id) = admin(&app).await;
    let id = store_session(&app, BANK, member_id, "plain-state").await;
    picsou::db::query("UPDATE sidecar_session SET sync_status = 'RUNNING' WHERE id = $1")
        .bind(id)
        .execute(&app.state.db)
        .await
        .unwrap();

    picsou::connectors::recover_interrupted_syncs(&app.state)
        .await
        .unwrap();

    let after = status(&client).await;
    assert_eq!(after["syncStatus"], "FAILED");
    assert_eq!(after["lastSyncError"], "INTERNAL_ERROR");
    assert_eq!(after["isActive"], true);
}

#[tokio::test]
async fn desktop_webview_login_persists_session_and_queues_sync() {
    let sidecar = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/webview/start"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "processId": "wv-1", "status": "WAITING_LOGIN"
        })))
        .mount(&sidecar)
        .await;
    Mock::given(method("GET"))
        .and(path("/webview/status/wv-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "DONE", "sessionState": "desktop-plain-state"
        })))
        .mount(&sidecar)
        .await;
    mount_accounts(
        &sidecar,
        json!([account(
            "PER",
            vec![position(
                Some("IE0000000001"),
                "IE0000000001",
                "10.5",
                "1260.00",
                "210.00"
            )],
            "1260.00"
        )]),
    )
    .await;
    let app = picsou::testing::TestApp::builder()
        .with_admin()
        .env(URL, sidecar.uri())
        .env("PICSOU_DESKTOP", "true")
        .spawn()
        .await;
    let (client, member_id) = admin(&app).await;

    assert_eq!(status(&client).await["webviewLoginSupported"], true);

    let started = client.post("/api/linxea/webview/start").send().await;
    assert_eq!(started.status, 200);
    assert_eq!(started.json()["processId"], "wv-1");

    let polled = client.get("/api/linxea/webview/status/wv-1").send().await;
    assert_eq!(polled.status, 200);
    assert_eq!(polled.json()["status"], "SYNCING");
    settle(&app).await;

    assert_eq!(status(&client).await["syncStatus"], "SUCCESS");
    assert!(session_of(&app, BANK, member_id).await.is_some());
}
