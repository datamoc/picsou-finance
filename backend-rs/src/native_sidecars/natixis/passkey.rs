//! `POST /passkey` (`{email}` -> `{sessionState}`): sign in with a passkey instead of a password
//! and a code.
//!
//! Natixis hands out a WebAuthn challenge, the desktop shell has the person's authenticator sign it
//! (they confirm in the shell's prompt), and the assertion goes back to Natixis. The login then
//! ends like a password one, with the same session state: no key or credential is kept. A server
//! has no way to show that prompt, so the route only works where a [`PasskeyBridge`] is set.

use std::collections::HashSet;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use http::StatusCode;
use http::header::ACCEPT;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;

use super::contract::{NatixisError, PasskeyRequest, SessionResponse};
use super::pending::LoginState;
use super::{
    NativeNatixis, NatixisConfig, check_upstream, classify_transport, format_status_message,
    read_json, serialize_session, status_message,
};
use crate::config::Config;
use crate::native_sidecars::common::{MAX_RESPONSE_BYTES, ReadBodyError, read_capped_bytes};
use crate::native_sidecars::{NativeError, NativeResponse};

/// The longest the shell keeps the passkey prompt open.
const MAX_PROMPT: Duration = Duration::from_secs(120);
/// The shell answers within [`MAX_PROMPT`]; the margin covers the prompt opening and closing.
const BRIDGE_TIMEOUT: Duration = Duration::from_secs(135);
/// The shell signs a challenge of 1024 bytes at most: this is its unpadded base64 length.
const MAX_CHALLENGE_CHARS: usize = 1366;
const API_KEY_HEADER: &str = "X-Picsou-Sidecar-Key";
const WHAT: &str = "passkey prompt";
/// The members every assertion carries.
const ASSERTION_MEMBERS: [&str; 4] = [
    "credentialId",
    "authenticatorData",
    "clientDataJson",
    "signature",
];
/// Natixis only serves what looks like its saver space open in a browser: without a browser's
/// `User-Agent` and `Accept-Language` it answers the passkey check with a 500. The browser named is
/// the one the flow was verified with, Chrome on Windows, whatever system the shell runs on.
const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36";

/// The desktop shell's loopback route that asks an authenticator for an assertion.
#[derive(Clone)]
pub struct PasskeyBridge {
    url: String,
    key: String,
    timeout: Duration,
}

impl std::fmt::Debug for PasskeyBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PasskeyBridge")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

impl PasskeyBridge {
    /// The bridge this install is configured with, if it is a usable one.
    pub fn from_config(config: &Config) -> Option<Self> {
        let url = config.natixis_passkey_url.as_deref()?;
        let bridge = Self::new(url, config.sidecar_api_key.as_deref().unwrap_or_default());
        if bridge.is_none() {
            tracing::warn!(
                "NATIXIS_PASSKEY_URL is ignored: it needs a loopback http URL and APP_SIDECAR_API_KEY"
            );
        }
        bridge
    }

    /// None unless `url` is plain HTTP on this machine and the shared key is set: the prompt it
    /// opens signs a bank login, so no other host may ever be asked.
    pub fn new(url: &str, key: &str) -> Option<Self> {
        let url = url.trim();
        (is_local_http(url) && !key.is_empty()).then(|| Self {
            url: format!("{}/assertion", url.trim_end_matches('/')),
            key: key.to_owned(),
            timeout: BRIDGE_TIMEOUT,
        })
    }
}

/// Plain HTTP to a loopback address, with no credentials, query or fragment. A name, even
/// `localhost`, would let the system's resolver choose where the key goes.
fn is_local_http(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    let loopback = match parsed.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(_)) | None => false,
    };
    loopback
        && parsed.scheme() == "http"
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.query().is_none()
        && parsed.fragment().is_none()
}

impl NatixisConfig {
    /// A config pointing every Natixis call at a stub on a loopback address, for tests; None for
    /// any other address, so no setting can send a saver's login elsewhere.
    pub fn for_local_stub(url: &str) -> Option<Self> {
        if is_local_http(url) {
            return Some(Self::for_mock_server(url));
        }
        if !url.is_empty() {
            tracing::warn!("NATIXIS_BASE_URL is ignored: it needs a loopback http URL");
        }
        None
    }
}

/// Natixis' answer to `GET /auth/v1/webauthn/authentication/options`, forwarded to the shell.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Options {
    rp_id: String,
    challenge: String,
    /// Left out, like `transports`, when Natixis sends none or `null`.
    #[serde(skip_serializing_if = "Option::is_none")]
    allow_credentials: Option<Vec<AllowedCredential>>,
    user_verification: Option<String>,
    /// Natixis calls it `timeout`, in milliseconds.
    #[serde(rename(deserialize = "timeout"))]
    timeout_ms: Option<u64>,
}

/// Only the two members the shell reads; Natixis also sends `type`.
#[derive(Debug, Deserialize, Serialize)]
struct AllowedCredential {
    id: String,
    /// Left out when Natixis names none: the shell takes a list or nothing, never `null`.
    #[serde(skip_serializing_if = "Option::is_none")]
    transports: Option<Vec<String>>,
}

/// The shell's assertion, binary fields as unpadded base64url.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Assertion {
    pub(crate) credential_id: String,
    pub(crate) authenticator_data: String,
    pub(crate) client_data_json: String,
    pub(crate) signature: String,
    pub(crate) user_handle: Option<String>,
}

impl NativeNatixis {
    pub(super) async fn handle_passkey(
        &self,
        body: Option<Value>,
    ) -> Result<NativeResponse, NativeError> {
        let req = PasskeyRequest::parse(body)?;
        let Some(bridge) = self.inner.config.passkey_bridge.clone() else {
            return Err(NativeError::new(
                StatusCode::NOT_IMPLEMENTED,
                "UPSTREAM_UNAVAILABLE",
            ));
        };
        let client = self
            .http_client_as(BROWSER_USER_AGENT)
            .map_err(NatixisError::into_native)?;
        let deadline = self.inner.config.route_deadline;

        let (state, options) = Self::run_with_deadline("passkey challenge", deadline, async {
            let state = self.open_login(&client).await?;
            let options = self.passkey_options(&client, &state, &req.email).await?;
            Ok((state, options))
        })
        .await?;
        // The person answers the prompt: its own budget, outside the Natixis deadlines.
        // The bridge times itself out, so that an unanswered prompt is not blamed on Natixis.
        let assertion = ask_the_shell(&bridge, &options).await.map_err(|err| {
            tracing::warn!(
                "Natixis passkey prompt failed code={}: {}",
                err.code,
                err.reason
            );
            err.into_native()
        })?;
        let session_state = Self::run_with_deadline("passkey login", deadline, async {
            self.verify_assertion(&client, &state, &assertion).await?;
            let finished = self.finish_login(&client, &state).await?;
            Ok(serialize_session(
                &finished.refresh_token,
                &state.device_id,
                &finished.employee_id,
            ))
        })
        .await?;

        Ok(NativeResponse::ok(
            serde_json::to_value(SessionResponse { session_state })
                .expect("SessionResponse serializes"),
        ))
    }

    /// Checks the account offers a passkey, then reads the challenge to sign.
    async fn passkey_options(
        &self,
        client: &reqwest::Client,
        state: &LoginState,
        email: &str,
    ) -> Result<Options, NatixisError> {
        let idp = &self.inner.config.idp_base;
        let response = client
            .post(format!("{idp}/auth/v1/authenticationModes/search"))
            .headers(self.idp_headers(&state.csrf, &state.device_id)?)
            .json(&json!({ "emailOrServerCode": email }))
            .send()
            .await
            .map_err(|err| classify_transport("login modes", &err))?;
        let status = response.status();
        check_upstream(status, "login modes")?;
        if status.is_client_error() {
            return Err(NatixisError::new(
                StatusCode::UNAUTHORIZED,
                "INVALID_CREDENTIALS",
                status_message("login modes", status, response).await,
            ));
        }
        let body = read_json(response, "login modes").await?;
        let modes: HashSet<&str> = body
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| item.get("mode")?.as_str())
            .collect();
        if modes.is_empty() {
            return Err(NatixisError::new(
                StatusCode::UNAUTHORIZED,
                "INVALID_CREDENTIALS",
                "No login mode for this e-mail",
            ));
        }
        if !modes.contains("WEBAUTHN") {
            return Err(NatixisError::new(
                StatusCode::NOT_IMPLEMENTED,
                "MFA_TYPE_UNSUPPORTED",
                "This account has no passkey login",
            ));
        }

        let response = client
            .get(format!("{idp}/auth/v1/webauthn/authentication/options"))
            .header("x-device-id", &state.device_id)
            .header(ACCEPT, "application/json, text/plain, */*")
            .send()
            .await
            .map_err(|err| classify_transport("passkey options", &err))?;
        let status = response.status();
        check_upstream(status, "passkey options")?;
        if status != StatusCode::OK {
            return Err(NatixisError::format_changed(
                status_message("passkey options", status, response).await,
            ));
        }
        let body = read_json(response, "passkey options").await?;
        let mut options: Options = serde_json::from_value(body).map_err(|_| {
            NatixisError::format_changed("passkey options have an unexpected shape")
        })?;
        // The key signs for the identity provider and no other site.
        let idp_host = Url::parse(idp)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned));
        if idp_host.as_deref() != Some(options.rp_id.as_str()) {
            return Err(NatixisError::format_changed(
                "passkey options name another relying party",
            ));
        }
        let challenge = options.challenge.trim_end_matches('=');
        if challenge.is_empty() || challenge.len() > MAX_CHALLENGE_CHARS {
            return Err(NatixisError::format_changed(
                "passkey options carry no usable challenge",
            ));
        }
        let longest = MAX_PROMPT.as_millis() as u64;
        options.timeout_ms = Some(options.timeout_ms.map_or(longest, |ms| ms.min(longest)));
        Ok(options)
    }

    async fn verify_assertion(
        &self,
        client: &reqwest::Client,
        state: &LoginState,
        assertion: &Assertion,
    ) -> Result<(), NatixisError> {
        // Natixis reads standard base64 with its padding, and refuses the base64url form.
        let standard = |value: &str| {
            URL_SAFE_NO_PAD
                .decode(value)
                .map(|bytes| STANDARD.encode(bytes))
                .map_err(|_| local_failure("the passkey answer is not base64url".to_owned()))
        };
        let mut signed = json!({
            "authenticatorData": standard(&assertion.authenticator_data)?,
            "clientDataJSON": standard(&assertion.client_data_json)?,
            "signature": standard(&assertion.signature)?,
        });
        if let Some(user_handle) = &assertion.user_handle {
            signed["userHandle"] = Value::String(standard(user_handle)?);
        }
        let response = client
            .post(format!(
                "{}/auth/v1/webauthn/authentication/verifications",
                self.inner.config.idp_base
            ))
            .headers(self.idp_headers(&state.csrf, &state.device_id)?)
            .json(&json!({
                "rawId": standard(&assertion.credential_id)?,
                "response": signed,
            }))
            .send()
            .await
            .map_err(|err| classify_transport("passkey check", &err))?;
        let status = response.status();
        check_upstream(status, "passkey check")?;
        if status == StatusCode::LOCKED {
            return Err(NatixisError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "RATE_LIMITED",
                "account locked",
            ));
        }
        if status.is_client_error() {
            return Err(NatixisError::new(
                StatusCode::UNAUTHORIZED,
                "INVALID_CREDENTIALS",
                status_message("passkey check", status, response).await,
            ));
        }
        if !matches!(status, StatusCode::OK | StatusCode::CREATED) {
            return Err(NatixisError::format_changed(
                status_message("passkey check", status, response).await,
            ));
        }
        Ok(())
    }
}

pub(crate) async fn ask_the_shell(
    bridge: &PasskeyBridge,
    options: &Options,
) -> Result<Assertion, NatixisError> {
    let client = reqwest::Client::builder()
        // Loopback only: a system proxy must never see the request or its key.
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(bridge.timeout)
        .build()
        .map_err(|err| local_failure(format!("failed to build the passkey client: {err}")))?;
    let response = client
        .post(&bridge.url)
        .header(API_KEY_HEADER, &bridge.key)
        .json(options)
        .send()
        .await
        .map_err(|err| transport_failure(&err))?;
    if response.status() != StatusCode::OK {
        return Err(refusal(response).await);
    }
    let bytes = read_capped_bytes(response, MAX_RESPONSE_BYTES)
        .await
        .map_err(|err| match err {
            ReadBodyError::Transport(err) => transport_failure(&err),
            ReadBodyError::TooLarge { limit } => {
                local_failure(format!("the passkey answer exceeded {limit} bytes"))
            }
        })?;
    let body: Value = serde_json::from_slice(&bytes)
        .map_err(|_| local_failure("the passkey answer is not JSON".to_owned()))?;
    // Names only: a parser message could quote the assertion.
    let mut wrong: Vec<&str> = ASSERTION_MEMBERS
        .into_iter()
        .filter(|member| !body[*member].is_string())
        .collect();
    if !matches!(body["userHandle"], Value::Null | Value::String(_)) {
        wrong.push("userHandle");
    }
    serde_json::from_value(body).map_err(|_| {
        local_failure(format!(
            "the passkey answer has missing or mistyped members: {wrong:?}"
        ))
    })
}

fn transport_failure(err: &reqwest::Error) -> NatixisError {
    let reason = classify_transport(WHAT, err).reason;
    // The shell closes the prompt first: a timeout here is a prompt nobody answered.
    if err.is_timeout() {
        attempt_expired(reason)
    } else {
        local_failure(reason)
    }
}

/// What the shell's refusal means for the person signing in.
async fn refusal(response: reqwest::Response) -> NatixisError {
    let status = response.status();
    let detail = read_json(response, WHAT)
        .await
        .ok()
        .and_then(|body| body["detail"].as_str().map(str::to_owned));
    let reason = format_status_message(WHAT, status, detail.as_slice());
    match status {
        // Closed, timed out, or one already open: the person can try again.
        StatusCode::GONE | StatusCode::REQUEST_TIMEOUT | StatusCode::CONFLICT => {
            attempt_expired(reason)
        }
        // This computer cannot show the prompt: trying again would not help.
        StatusCode::NOT_IMPLEMENTED => NatixisError::new(status, "MFA_TYPE_UNSUPPORTED", reason),
        // The shell refuses to sign what Natixis asks for.
        StatusCode::BAD_REQUEST | StatusCode::FORBIDDEN => NatixisError::format_changed(reason),
        _ => local_failure(reason),
    }
}

fn attempt_expired(reason: String) -> NatixisError {
    NatixisError::new(StatusCode::GONE, "AUTH_ATTEMPT_EXPIRED", reason)
}

/// A failure between Picsou and its own shell: the bank has no part in it.
fn local_failure(reason: String) -> NatixisError {
    NatixisError::new(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR", reason)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    const KEY: &str = "shared-key-value";

    /// What Natixis sends, with and without a named transport.
    fn options() -> Options {
        serde_json::from_value(json!({
            "rpId": "idp.example",
            "challenge": "Y2hhbGxlbmdl",
            "timeout": 60000,
            "allowCredentials": [
                { "type": "public-key", "id": "Y3JlZA" },
                { "type": "public-key", "id": "a2V5", "transports": ["usb"] },
            ],
            "userVerification": "required"
        }))
        .expect("options")
    }

    /// A fake shell that gives `reply`.
    async fn shell(reply: ResponseTemplate) -> (MockServer, PasskeyBridge) {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/passkey/assertion"))
            .respond_with(reply)
            .mount(&server)
            .await;
        let bridge = PasskeyBridge::new(&format!("{}/passkey", server.uri()), KEY).expect("bridge");
        (server, bridge)
    }

    async fn ask(reply: ResponseTemplate) -> (MockServer, Result<Assertion, NatixisError>) {
        let (server, bridge) = shell(reply).await;
        let answer = ask_the_shell(&bridge, &options()).await;
        (server, answer)
    }

    fn assertion() -> Value {
        json!({
            "credentialId": "Y3JlZA",
            "authenticatorData": "AQ",
            "clientDataJson": "e30",
            "signature": "Ag",
            "userHandle": "Aw",
        })
    }

    #[test]
    fn the_bridge_is_only_ever_on_this_machine() {
        for url in ["http://127.0.0.1:5000/passkey", "http://[::1]:5000/passkey"] {
            assert!(PasskeyBridge::new(url, "key").is_some(), "{url}");
        }
        for url in [
            "https://example.com/passkey",
            "https://127.0.0.1/passkey",
            "http://localhost:5000/passkey",
            "http://127.0.0.1/passkey#x",
            "http://192.168.1.10/passkey",
            "http://127.0.0.1.example.com/passkey",
            "http://user:pw@127.0.0.1/passkey",
            "http://127.0.0.1/passkey?x=1",
            "not a url",
        ] {
            assert!(PasskeyBridge::new(url, "key").is_none(), "{url}");
        }
        assert!(PasskeyBridge::new("http://127.0.0.1:5000/passkey", "").is_none());
        let padded = PasskeyBridge::new(" http://127.0.0.1:5000/passkey/ ", "key").expect("bridge");
        assert_eq!(padded.url, "http://127.0.0.1:5000/passkey/assertion");
    }

    #[test]
    fn the_address_override_only_ever_points_at_this_machine() {
        assert!(NatixisConfig::for_local_stub("http://127.0.0.1:5000").is_some());
        for url in [
            "",
            "https://example.com",
            "http://192.168.1.10:5000",
            "http://localhost:5000",
        ] {
            assert!(NatixisConfig::for_local_stub(url).is_none(), "{url}");
        }
    }

    #[test]
    fn the_bridge_never_prints_its_key() {
        let bridge =
            PasskeyBridge::new("http://127.0.0.1:5000/passkey", "very-secret-key").expect("bridge");

        assert!(!format!("{bridge:?}").contains("very-secret-key"));
    }

    #[tokio::test]
    async fn the_shell_gets_the_options_with_the_key_and_answers_the_assertion() {
        let (server, answer) = ask(ResponseTemplate::new(200).set_body_json(assertion())).await;

        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests[0].headers[API_KEY_HEADER], KEY);
        let sent: Value = serde_json::from_slice(&requests[0].body).expect("JSON");
        assert_eq!(
            sent,
            json!({
                "rpId": "idp.example",
                "challenge": "Y2hhbGxlbmdl",
                "allowCredentials": [
                    { "id": "Y3JlZA" },
                    { "id": "a2V5", "transports": ["usb"] },
                ],
                "userVerification": "required",
                "timeoutMs": 60000,
            })
        );
        let answer = answer.expect("assertion");
        assert_eq!(answer.credential_id, "Y3JlZA");
        assert_eq!(answer.authenticator_data, "AQ");
        assert_eq!(answer.client_data_json, "e30");
        assert_eq!(answer.signature, "Ag");
        assert_eq!(answer.user_handle.as_deref(), Some("Aw"));
    }

    #[tokio::test]
    async fn a_prompt_that_was_not_confirmed_can_be_tried_again() {
        for status in [410, 408, 409] {
            let (_server, answer) = ask(ResponseTemplate::new(status)).await;

            let refused = answer.err().expect("refused");
            assert_eq!(refused.code, "AUTH_ATTEMPT_EXPIRED", "{status}");
            assert_eq!(refused.status, StatusCode::GONE, "{status}");
        }
    }

    #[test]
    fn a_missing_or_null_credential_list_is_not_forwarded() {
        for options in [
            json!({ "rpId": "idp.example", "challenge": "AA" }),
            json!({ "rpId": "idp.example", "challenge": "AA", "allowCredentials": null }),
        ] {
            let options: Options = serde_json::from_value(options).expect("options");

            let sent = serde_json::to_value(options).expect("JSON");

            assert!(sent.get("allowCredentials").is_none(), "{sent}");
        }
    }

    #[tokio::test]
    async fn each_other_answer_says_where_the_fault_is() {
        for (reply, code) in [
            (ResponseTemplate::new(501), "MFA_TYPE_UNSUPPORTED"),
            (ResponseTemplate::new(400), "UPSTREAM_FORMAT_CHANGED"),
            (ResponseTemplate::new(403), "UPSTREAM_FORMAT_CHANGED"),
            (ResponseTemplate::new(401), "INTERNAL_ERROR"),
            (ResponseTemplate::new(500), "INTERNAL_ERROR"),
            (
                ResponseTemplate::new(200).set_body_string("<html>"),
                "INTERNAL_ERROR",
            ),
        ] {
            let (_server, answer) = ask(reply).await;

            let refused = answer.err().expect("refused");
            assert_eq!(refused.code, code, "{}", refused.reason);
        }
    }

    #[tokio::test]
    async fn the_log_reason_names_the_cause_but_no_assertion_value() {
        let refusal = json!({ "detail": "PASSKEY_UNAVAILABLE" });
        let (_server, answer) = ask(ResponseTemplate::new(501).set_body_json(refusal)).await;
        let reason = answer.err().expect("refused").reason;
        assert!(reason.contains("PASSKEY_UNAVAILABLE"), "{reason}");

        let partial = json!({ "signature": "secret-signature" });
        let (_server, answer) = ask(ResponseTemplate::new(200).set_body_json(partial)).await;
        let reason = answer.err().expect("refused").reason;
        assert!(reason.contains("credentialId"), "{reason}");
        assert!(!reason.contains("secret-signature"), "{reason}");

        let mut mistyped = assertion();
        mistyped["userHandle"] = json!(5);
        let (_server, answer) = ask(ResponseTemplate::new(200).set_body_json(mistyped)).await;
        let reason = answer.err().expect("refused").reason;
        assert!(reason.contains("userHandle"), "{reason}");
    }

    #[tokio::test]
    async fn a_prompt_nobody_answers_expires() {
        let slow = ResponseTemplate::new(200).set_delay(Duration::from_secs(5));
        let (_server, mut bridge) = shell(slow).await;
        bridge.timeout = Duration::from_millis(50);

        let answer = ask_the_shell(&bridge, &options()).await;

        assert_eq!(answer.err().expect("refused").code, "AUTH_ATTEMPT_EXPIRED");
    }

    #[tokio::test]
    async fn an_answer_that_stalls_halfway_expires_too() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let url = format!("http://{}/passkey", listener.local_addr().expect("address"));
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("connection");
            let _ = socket.read(&mut [0; 4096]).await.expect("request");
            let headers = b"HTTP/1.1 200 OK\r\ncontent-length: 10\r\n\r\n";
            socket.write_all(headers).await.expect("headers");
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let mut bridge = PasskeyBridge::new(&url, KEY).expect("bridge");
        bridge.timeout = Duration::from_millis(500);

        let answer = ask_the_shell(&bridge, &options()).await;

        assert_eq!(answer.err().expect("refused").code, "AUTH_ATTEMPT_EXPIRED");
    }

    #[tokio::test]
    async fn a_shell_that_cannot_be_reached_is_not_the_banks_fault() {
        let bridge = PasskeyBridge::new("http://127.0.0.1:1/passkey", KEY).expect("bridge");

        let answer = ask_the_shell(&bridge, &options()).await;

        assert_eq!(answer.err().expect("refused").code, "INTERNAL_ERROR");
    }

    /// Following one would hand the key and the challenge to whatever it names.
    #[tokio::test]
    async fn a_redirect_is_never_followed() {
        let elsewhere = ResponseTemplate::new(307).insert_header("location", "/elsewhere");

        let (server, answer) = ask(elsewhere).await;

        assert_eq!(answer.err().expect("refused").code, "INTERNAL_ERROR");
        assert_eq!(server.received_requests().await.expect("requests").len(), 1);
    }
}
