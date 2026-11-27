//! Natixis passkey sign-in: a fake Natixis and a fake desktop shell. Every value is synthetic.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use http::{Method, StatusCode};
use picsou::native_sidecars::natixis::{
    DEFAULT_REDIRECT_URI, NativeNatixis, NatixisConfig, PasskeyBridge, parse_session,
};
use picsou::testing::TestApp;
use serde_json::{Value, json};
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const EMAIL: &str = "saver@example.org";
const KEY: &str = "shared-key-value";
const CSRF: &str = "csrf-value-0123456789";
const AUTH_CODE: &str = "authorization-code-value";
const REFRESH_TOKEN: &str = "refresh-token-value";
const EMPLOYEE_ID: &str = "10000-1000000000001";
const SHELL_PATH: &str = "/passkey/assertion";

#[derive(Clone)]
struct Seen {
    method: String,
    path: String,
    headers: http::HeaderMap,
    body: Vec<u8>,
}

impl Seen {
    fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("JSON body")
    }
}

struct Behaviour {
    modes: Value,
    credential: Value,
    rp_id: Option<String>,
    challenge: String,
    shell: (u16, Value),
    shell_delay: Duration,
    verification_status: u16,
    oauth_state: String,
    requests: Vec<Seen>,
}

#[derive(Clone)]
struct Fake {
    base: String,
    behaviour: Arc<Mutex<Behaviour>>,
}

impl Respond for Fake {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let method = request.method.as_str().to_owned();
        let path = request.url.path().to_owned();
        let mut b = self.behaviour.lock().unwrap();
        b.requests.push(Seen {
            method: method.clone(),
            path: path.clone(),
            headers: request.headers.clone(),
            body: request.body.clone(),
        });
        let json_with =
            |status: u16, body: Value| ResponseTemplate::new(status).set_body_json(body);
        match (method.as_str(), path.as_str()) {
            ("GET", "/api/oauth/authorize") => {
                b.oauth_state = request
                    .url
                    .query_pairs()
                    .find(|(k, _)| k == "state")
                    .map(|(_, v)| v.into_owned())
                    .unwrap_or_default();
                let html = format!(
                    r#"<form method="post" action="{}/saml/sso"><input type="hidden" name="SAMLRequest" value="request"/></form>"#,
                    self.base
                );
                ResponseTemplate::new(200).set_body_raw(html, "text/html")
            }
            ("POST", "/saml/sso") => ResponseTemplate::new(200).set_body_raw(
                format!(r#"<meta name="_csrf" content="{CSRF}"/>"#),
                "text/html",
            ),
            ("POST", "/auth/v1/authenticationModes/search") => json_with(200, b.modes.clone()),
            ("GET", "/auth/v1/webauthn/authentication/options") => {
                let rp_id = b.rp_id.clone().unwrap_or_else(|| "127.0.0.1".to_owned());
                json_with(
                    200,
                    json!({
                        "challenge": b.challenge,
                        "timeout": 600000,
                        "rpId": rp_id,
                        "allowCredentials": [b.credential.clone()],
                        "userVerification": "required"
                    }),
                )
            }
            ("POST", SHELL_PATH) => {
                json_with(b.shell.0, b.shell.1.clone()).set_delay(b.shell_delay)
            }
            ("POST", "/auth/v1/webauthn/authentication/verifications") => {
                json_with(b.verification_status, json!({}))
            }
            ("POST", "/auth/v1/profiles/me/verifications") => json_with(
                201,
                json!({ "saml": {
                    "encodedValue": "saml-response-value",
                    "serviceProviderUrl": format!("{}/api/oauth/authorize", self.base)
                } }),
            ),
            ("POST", "/api/oauth/authorize") => ResponseTemplate::new(302).insert_header(
                "location",
                format!(
                    "{DEFAULT_REDIRECT_URI}?code={AUTH_CODE}&state={}",
                    b.oauth_state
                ),
            ),
            ("POST", "/api/oauth/token") => json_with(
                200,
                json!({ "access_token": "access-token-value", "refresh_token": REFRESH_TOKEN }),
            ),
            ("GET", "/auth/v3/users/me") => json_with(200, json!({ "id": EMPLOYEE_ID })),
            _ => ResponseTemplate::new(404),
        }
    }
}

/// A fake Natixis and, on the same server, a fake desktop shell that confirms the prompt.
async fn fake_natixis() -> (MockServer, Arc<Mutex<Behaviour>>) {
    let server = MockServer::start().await;
    let behaviour = Arc::new(Mutex::new(Behaviour {
        modes: json!({ "items": [{ "mode": "PASSWORD" }, { "mode": "WEBAUTHN" }] }),
        credential: json!({ "type": "public-key", "id": "Y3JlZA", "transports": ["internal"] }),
        rp_id: None,
        challenge: "Y2hhbGxlbmdl".to_owned(),
        // base64url of bytes whose standard form needs `+`, `/` and padding.
        shell: (
            200,
            json!({
                "credentialId": "-_8",
                "authenticatorData": "AQ",
                "clientDataJson": "e30",
                "signature": "-_8",
            }),
        ),
        shell_delay: Duration::ZERO,
        verification_status: 200,
        oauth_state: String::new(),
        requests: Vec::new(),
    }));
    Mock::given(any())
        .respond_with(Fake {
            base: server.uri(),
            behaviour: behaviour.clone(),
        })
        .mount(&server)
        .await;
    (server, behaviour)
}

struct Harness {
    _server: MockServer,
    behaviour: Arc<Mutex<Behaviour>>,
    sidecar: NativeNatixis,
}

impl Harness {
    async fn start(with_shell: bool) -> Self {
        let (server, behaviour) = fake_natixis().await;
        let mut config = NatixisConfig::for_mock_server(&server.uri());
        if with_shell {
            let url = format!("{}/passkey", server.uri());
            config = config.with_passkey_bridge(PasskeyBridge::new(&url, KEY).expect("bridge"));
        }
        Self {
            _server: server,
            behaviour,
            sidecar: NativeNatixis::with_config(config),
        }
    }

    fn change(&self, change: impl FnOnce(&mut Behaviour)) {
        change(&mut self.behaviour.lock().unwrap());
    }

    fn requests(&self) -> Vec<Seen> {
        self.behaviour.lock().unwrap().requests.clone()
    }

    fn sent(&self, path: &str) -> Option<Seen> {
        self.requests().into_iter().find(|r| r.path == path)
    }

    async fn sign_in(&self) -> Result<Value, (StatusCode, String)> {
        self.sidecar
            .call(Method::POST, "/passkey", Some(json!({ "email": EMAIL })))
            .await
    }
}

fn refused(status: StatusCode, code: &str) -> (StatusCode, String) {
    (status, code.to_owned())
}

#[tokio::test]
async fn a_passkey_sign_in_ends_with_the_same_session_as_a_password_one() {
    let h = Harness::start(true).await;

    let body = h.sign_in().await.expect("session");

    let session = parse_session(body["sessionState"].as_str().unwrap()).expect("state");
    assert_eq!(session.refresh_token, REFRESH_TOKEN);
    assert_eq!(session.employee_id, EMPLOYEE_ID);
    let paths: Vec<String> = h.requests().into_iter().map(|r| r.path).collect();
    assert_eq!(
        paths,
        [
            "/api/oauth/authorize",
            "/saml/sso",
            "/auth/v1/authenticationModes/search",
            "/auth/v1/webauthn/authentication/options",
            SHELL_PATH,
            "/auth/v1/webauthn/authentication/verifications",
            "/auth/v1/profiles/me/verifications",
            "/api/oauth/authorize",
            "/api/oauth/token",
            "/auth/v3/users/me",
        ]
    );
}

/// The shell refuses `"transports": null`, so an absent list stays absent.
#[tokio::test]
async fn a_credential_without_transports_reaches_the_shell_without_the_member() {
    let h = Harness::start(true).await;
    h.change(|b| b.credential = json!({ "type": "public-key", "id": "Y3JlZA" }));

    h.sign_in().await.expect("session");

    assert_eq!(
        h.sent(SHELL_PATH).expect("shell asked").json()["allowCredentials"],
        json!([{ "id": "Y3JlZA" }])
    );
}

#[tokio::test]
async fn natixis_only_ever_sees_a_browser() {
    let h = Harness::start(true).await;

    h.sign_in().await.expect("session");

    for request in h.requests().iter().filter(|r| r.path != SHELL_PATH) {
        let agent = request.header("user-agent");
        assert!(
            agent.starts_with("Mozilla/5.0 (Windows NT 10.0; Win64; x64)"),
            "{} {} sent {agent:?}",
            request.method,
            request.path
        );
        assert_eq!(
            request.header("accept-language"),
            "fr-FR,fr;q=0.9",
            "{}",
            request.path
        );
    }
}

#[tokio::test]
async fn the_shell_gets_the_challenge_with_its_key_and_a_bounded_prompt() {
    let h = Harness::start(true).await;

    h.sign_in().await.expect("session");

    let asked = h.sent(SHELL_PATH).expect("shell asked");
    assert_eq!(asked.header("x-picsou-sidecar-key"), KEY);
    assert_eq!(
        asked.json(),
        json!({
            "rpId": "127.0.0.1",
            "challenge": "Y2hhbGxlbmdl",
            "allowCredentials": [{ "id": "Y3JlZA", "transports": ["internal"] }],
            "userVerification": "required",
            "timeoutMs": 120000
        })
    );
    for request in h.requests().iter().filter(|r| r.path != SHELL_PATH) {
        assert_eq!(request.header("x-picsou-sidecar-key"), "");
    }
}

#[tokio::test]
async fn the_assertion_reaches_natixis_in_padded_standard_base64() {
    let h = Harness::start(true).await;

    h.sign_in().await.expect("session");

    let check = h
        .sent("/auth/v1/webauthn/authentication/verifications")
        .expect("checked");
    assert_eq!(
        check.json(),
        json!({
            "rawId": "+/8=",
            "response": {
                "authenticatorData": "AQ==",
                "clientDataJSON": "e30=",
                "signature": "+/8="
            }
        })
    );
    assert_eq!(check.header("x-csrf-token"), CSRF);
    assert!(!check.header("x-device-id").is_empty());
}

#[tokio::test]
async fn a_user_handle_is_forwarded_when_the_key_returns_one() {
    let h = Harness::start(true).await;
    h.change(|b| b.shell.1["userHandle"] = json!("-_8"));

    h.sign_in().await.expect("session");

    let check = h
        .sent("/auth/v1/webauthn/authentication/verifications")
        .expect("checked");
    assert_eq!(check.json()["response"]["userHandle"], "+/8=");
}

#[tokio::test]
async fn an_account_without_a_passkey_opens_no_prompt() {
    let h = Harness::start(true).await;
    h.change(|b| b.modes = json!({ "items": [{ "mode": "PASSWORD" }] }));

    let outcome = h.sign_in().await.unwrap_err();

    assert_eq!(
        outcome,
        refused(StatusCode::NOT_IMPLEMENTED, "MFA_TYPE_UNSUPPORTED")
    );
    assert!(h.sent(SHELL_PATH).is_none());
}

#[tokio::test]
async fn an_unknown_e_mail_opens_no_prompt() {
    let h = Harness::start(true).await;
    h.change(|b| b.modes = json!({ "items": [] }));

    let outcome = h.sign_in().await.unwrap_err();

    assert_eq!(
        outcome,
        refused(StatusCode::UNAUTHORIZED, "INVALID_CREDENTIALS")
    );
    assert!(h.sent(SHELL_PATH).is_none());
}

#[tokio::test]
async fn another_relying_party_is_never_signed_for() {
    let h = Harness::start(true).await;
    h.change(|b| b.rp_id = Some("example.com".to_owned()));

    let outcome = h.sign_in().await.unwrap_err();

    assert_eq!(
        outcome,
        refused(StatusCode::BAD_GATEWAY, "UPSTREAM_FORMAT_CHANGED")
    );
    assert!(h.sent(SHELL_PATH).is_none());
}

#[tokio::test]
async fn a_prompt_that_was_not_confirmed_sends_nothing_to_natixis() {
    for (status, detail) in [
        (410, "PASSKEY_CANCELLED"),
        (408, "PASSKEY_TIMEOUT"),
        (409, "PASSKEY_BUSY"),
    ] {
        let h = Harness::start(true).await;
        h.change(|b| b.shell = (status, json!({ "detail": detail })));

        let outcome = h.sign_in().await.unwrap_err();

        assert_eq!(
            outcome,
            refused(StatusCode::GONE, "AUTH_ATTEMPT_EXPIRED"),
            "{detail}"
        );
        assert_eq!(h.requests().last().unwrap().path, SHELL_PATH);
    }
}

#[tokio::test]
async fn a_shell_failure_is_never_blamed_on_natixis() {
    for (status, detail, expected) in [
        (
            501,
            "PASSKEY_UNAVAILABLE",
            refused(StatusCode::NOT_IMPLEMENTED, "MFA_TYPE_UNSUPPORTED"),
        ),
        (
            500,
            "PASSKEY_FAILED",
            refused(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR"),
        ),
    ] {
        let h = Harness::start(true).await;
        h.change(|b| b.shell = (status, json!({ "detail": detail })));

        let outcome = h.sign_in().await.unwrap_err();

        assert_eq!(outcome, expected, "{detail}");
        assert_eq!(h.requests().last().unwrap().path, SHELL_PATH);
    }
}

#[tokio::test]
async fn an_unreadable_assertion_is_a_local_failure() {
    let h = Harness::start(true).await;
    h.change(|b| b.shell.1["signature"] = json!("***"));

    let outcome = h.sign_in().await.unwrap_err();

    assert_eq!(
        outcome,
        refused(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR")
    );
    assert!(
        h.sent("/auth/v1/webauthn/authentication/verifications")
            .is_none()
    );
}

/// The shell signs 1024 bytes at most, which base64 writes in 1366 characters before padding.
#[tokio::test]
async fn a_challenge_the_shell_would_refuse_opens_no_prompt() {
    for (challenge, prompted) in [
        (format!("{}==", "A".repeat(1366)), true),
        ("A".repeat(1367), false),
        ("==".to_owned(), false),
    ] {
        let h = Harness::start(true).await;
        h.change(|b| b.challenge = challenge.clone());

        let outcome = h.sign_in().await;

        assert_eq!(
            h.sent(SHELL_PATH).is_some(),
            prompted,
            "{}",
            challenge.len()
        );
        if !prompted {
            assert_eq!(
                outcome.unwrap_err(),
                refused(StatusCode::BAD_GATEWAY, "UPSTREAM_FORMAT_CHANGED")
            );
        }
    }
}

#[tokio::test]
async fn a_refused_passkey_stops_before_the_profile_check() {
    for (status, expected) in [
        (
            400,
            refused(StatusCode::UNAUTHORIZED, "INVALID_CREDENTIALS"),
        ),
        (423, refused(StatusCode::TOO_MANY_REQUESTS, "RATE_LIMITED")),
        (
            500,
            refused(StatusCode::BAD_GATEWAY, "UPSTREAM_UNAVAILABLE"),
        ),
    ] {
        let h = Harness::start(true).await;
        h.change(|b| b.verification_status = status);

        let outcome = h.sign_in().await.unwrap_err();

        assert_eq!(outcome, expected, "{status}");
        assert!(h.sent("/auth/v1/profiles/me/verifications").is_none());
    }
}

#[tokio::test]
async fn without_a_shell_nothing_is_sent_at_all() {
    let h = Harness::start(false).await;

    let outcome = h.sign_in().await.unwrap_err();

    assert_eq!(
        outcome,
        refused(StatusCode::NOT_IMPLEMENTED, "UPSTREAM_UNAVAILABLE")
    );
    assert!(h.requests().is_empty());
}

#[tokio::test]
async fn the_request_holds_an_e_mail_and_nothing_else() {
    let h = Harness::start(true).await;

    for body in [
        json!({}),
        json!({ "email": "" }),
        json!({ "email": EMAIL, "password": "pw" }),
    ] {
        let outcome = h
            .sidecar
            .call(Method::POST, "/passkey", Some(body.clone()))
            .await
            .unwrap_err();

        assert_eq!(
            outcome,
            refused(StatusCode::BAD_REQUEST, "INVALID_DATA"),
            "{body}"
        );
    }
    assert!(h.requests().is_empty());
}

// ─── Routes ──────────────────────────────────────────────────────────────────

const PASSKEY_ROUTE: &str = "/api/natixis/auth/passkey";

/// A set-up app whose in-process Natixis connector talks to the fake, with or without a shell.
async fn desktop_app(server: &MockServer, with_shell: bool) -> TestApp {
    let mut app = TestApp::builder()
        .with_admin()
        .env("NATIXIS_AUTH_URL", "native://natixis-auth")
        .env("NATIXIS_BASE_URL", server.uri())
        .env("APP_SIDECAR_API_KEY", KEY);
    if with_shell {
        app = app.env("NATIXIS_PASSKEY_URL", format!("{}/passkey", server.uri()));
    }
    app.spawn().await
}

#[tokio::test]
async fn a_passkey_login_stores_the_session_and_queues_a_sync() {
    let (server, _behaviour) = fake_natixis().await;
    let app = desktop_app(&server, true).await;
    let client = app.login_admin().await;
    assert_eq!(
        client.get(PASSKEY_ROUTE).send().await.json(),
        json!({ "available": true })
    );

    let response = client
        .post(PASSKEY_ROUTE)
        .json(&json!({ "email": EMAIL }))
        .send()
        .await;

    assert_eq!(response.status, 200, "{}", response.text());
    assert_eq!(response.json()["isActive"], true);
    app.state.connectors.jobs.wait_idle().await;
    let stored: String = picsou::db::query_scalar(
        "SELECT session_state FROM sidecar_session WHERE bank = 'NATIXIS'",
    )
    .fetch_one(&app.state.db)
    .await
    .expect("session row");
    let plain = app.state.crypto.decrypt(&stored).expect("decrypt");
    assert_eq!(
        parse_session(&plain).expect("state").refresh_token,
        REFRESH_TOKEN
    );
}

#[tokio::test]
async fn an_install_without_the_prompt_offers_and_runs_no_passkey_login() {
    let (server, behaviour) = fake_natixis().await;
    let without_shell = desktop_app(&server, false).await;
    // A sidecar over HTTP has no passkey route, whatever else is configured.
    let http_sidecar = TestApp::builder()
        .with_admin()
        .env("NATIXIS_AUTH_URL", server.uri())
        .env("APP_SIDECAR_API_KEY", KEY)
        .env("NATIXIS_PASSKEY_URL", format!("{}/passkey", server.uri()))
        .spawn()
        .await;

    for app in [without_shell, http_sidecar] {
        let client = app.login_admin().await;

        let availability = client.get(PASSKEY_ROUTE).send().await.json();
        let response = client
            .post(PASSKEY_ROUTE)
            .json(&json!({ "email": EMAIL }))
            .send()
            .await;

        assert_eq!(availability, json!({ "available": false }));
        assert_eq!(response.json()["code"], "UPSTREAM_UNAVAILABLE");
    }
    assert!(behaviour.lock().unwrap().requests.is_empty());
}

#[tokio::test]
async fn the_passkey_routes_need_a_signed_in_member_and_an_e_mail() {
    let (server, behaviour) = fake_natixis().await;
    let app = desktop_app(&server, true).await;
    let client = app.login_admin().await;

    let blank = client
        .post(PASSKEY_ROUTE)
        .json(&json!({ "email": " " }))
        .send()
        .await;
    let anonymous = reqwest::get(format!("{}{PASSKEY_ROUTE}", app.base_url))
        .await
        .expect("request");

    assert_eq!(blank.status, 422, "{}", blank.text());
    assert_eq!(blank.json()["errors"]["email"], "must not be blank");
    assert_eq!(anonymous.status().as_u16(), 401);
    assert!(behaviour.lock().unwrap().requests.is_empty());
}

/// A passkey sign-in is a sign-in: it draws from the same budget as a password one.
#[tokio::test]
async fn passkey_and_password_sign_ins_share_one_attempt_budget() {
    let (server, _behaviour) = fake_natixis().await;
    let app = desktop_app(&server, false).await;
    let client = app.login_admin().await;
    let passkey = json!({ "email": EMAIL });

    for _ in 0..5 {
        let response = client.post(PASSKEY_ROUTE).json(&passkey).send().await;
        assert_eq!(response.status, 422, "{}", response.text());
    }
    let sixth = client.post(PASSKEY_ROUTE).json(&passkey).send().await;
    let password = client
        .post("/api/natixis/auth/initiate")
        .json(&json!({ "email": EMAIL, "password": "pw" }))
        .send()
        .await;

    assert_eq!(sixth.status, 429, "{}", sixth.text());
    assert_eq!(password.status, 429, "{}", password.text());
}

#[tokio::test]
async fn a_disconnect_while_the_prompt_is_open_voids_the_login() {
    let (server, behaviour) = fake_natixis().await;
    behaviour.lock().unwrap().shell_delay = Duration::from_millis(1500);
    let app = desktop_app(&server, true).await;
    let signing_in = app.login_admin().await;
    let disconnecting = app.login_admin().await;

    let (login, disconnect) = tokio::join!(
        async {
            signing_in
                .post(PASSKEY_ROUTE)
                .json(&json!({ "email": EMAIL }))
                .send()
                .await
        },
        async {
            while !behaviour
                .lock()
                .unwrap()
                .requests
                .iter()
                .any(|r| r.path == SHELL_PATH)
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            disconnecting.delete("/api/natixis/session").send().await
        }
    );

    assert_eq!(disconnect.status, 204);
    assert_eq!(login.json()["code"], "AUTH_ATTEMPT_EXPIRED");
    let sessions: i64 = picsou::db::query_scalar("SELECT count(*) FROM sidecar_session")
        .fetch_one(&app.state.db)
        .await
        .expect("count");
    assert_eq!(sessions, 0);
}
