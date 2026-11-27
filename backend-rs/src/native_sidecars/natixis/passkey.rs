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
    NativeNatixis, NatixisConfig, check_upstream, classify_transport, read_json, serialize_session,
    status_message,
};
use crate::config::Config;
use crate::native_sidecars::{NativeError, NativeResponse};

/// The longest the shell keeps the passkey prompt open.
const MAX_PROMPT: Duration = Duration::from_secs(120);
/// The shell answers within [`MAX_PROMPT`]; the margin covers the prompt opening and closing.
const BRIDGE_TIMEOUT: Duration = Duration::from_secs(135);
const MAX_CHALLENGE_CHARS: usize = 2048;
const API_KEY_HEADER: &str = "X-Picsou-Sidecar-Key";
/// Natixis only serves what looks like its saver space open in a browser: without a browser's
/// `User-Agent` and `Accept-Language` it answers the passkey check with a 500. The browser named is
/// the one the flow was verified with, Chrome on Windows, whatever system the shell runs on.
const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36";

/// The desktop shell's loopback route that asks an authenticator for an assertion.
#[derive(Clone)]
pub struct PasskeyBridge {
    url: String,
    key: String,
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
        (is_local_http(url) && !key.is_empty()).then(|| Self {
            url: format!("{}/assertion", url.trim_end_matches('/')),
            key: key.to_owned(),
        })
    }
}

/// Plain HTTP on this machine, with no credentials, query or fragment.
fn is_local_http(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    let loopback = match parsed.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(name)) => name == "localhost",
        None => false,
    };
    loopback
        && parsed.scheme() == "http"
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.query().is_none()
        && parsed.fragment().is_none()
}

impl NatixisConfig {
    /// A config pointing every Natixis call at a stub on this machine, for tests; None for any
    /// other address, so no setting can send a saver's login elsewhere.
    pub fn for_local_stub(url: &str) -> Option<Self> {
        is_local_http(url).then(|| Self::for_mock_server(url))
    }
}

/// Natixis' answer to `GET /auth/v1/webauthn/authentication/options`, forwarded to the shell.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Options {
    rp_id: String,
    challenge: String,
    #[serde(default)]
    allow_credentials: Vec<AllowedCredential>,
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
struct Assertion {
    credential_id: String,
    authenticator_data: String,
    client_data_json: String,
    signature: String,
    user_handle: Option<String>,
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
        let assertion = Self::run_with_deadline(
            "passkey prompt",
            BRIDGE_TIMEOUT,
            ask_the_shell(&bridge, &options),
        )
        .await?;
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
        if options.challenge.is_empty() || options.challenge.len() > MAX_CHALLENGE_CHARS {
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
                .map_err(|_| NatixisError::unavailable("the passkey answer is not base64url"))
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

async fn ask_the_shell(
    bridge: &PasskeyBridge,
    options: &Options,
) -> Result<Assertion, NatixisError> {
    let client = reqwest::Client::builder()
        // Loopback only: a system proxy must never see the request or its key.
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(BRIDGE_TIMEOUT)
        .build()
        .map_err(|_| NatixisError::unavailable("failed to build the passkey client"))?;
    let response = client
        .post(&bridge.url)
        .header(API_KEY_HEADER, &bridge.key)
        .json(options)
        .send()
        .await
        .map_err(|err| classify_transport("passkey prompt", &err))?;
    let status = response.status();
    // Closed, timed out, or one already open: the person can try again.
    if matches!(
        status,
        StatusCode::GONE | StatusCode::REQUEST_TIMEOUT | StatusCode::CONFLICT
    ) {
        return Err(NatixisError::new(
            StatusCode::GONE,
            "AUTH_ATTEMPT_EXPIRED",
            format!("passkey prompt answered {}", status.as_u16()),
        ));
    }
    if status != StatusCode::OK {
        return Err(NatixisError::unavailable(format!(
            "passkey prompt answered {}",
            status.as_u16()
        )));
    }
    let body = read_json(response, "passkey prompt").await?;
    serde_json::from_value(body)
        .map_err(|_| NatixisError::unavailable("the passkey answer has an unexpected shape"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bridge_is_only_ever_on_this_machine() {
        for url in [
            "http://127.0.0.1:5000/passkey",
            "http://localhost:5000/passkey",
            "http://[::1]:5000/passkey",
        ] {
            assert!(PasskeyBridge::new(url, "key").is_some(), "{url}");
        }
        for url in [
            "https://example.com/passkey",
            "http://192.168.1.10/passkey",
            "http://127.0.0.1.example.com/passkey",
            "http://user:pw@127.0.0.1/passkey",
            "http://127.0.0.1/passkey?x=1",
            "not a url",
        ] {
            assert!(PasskeyBridge::new(url, "key").is_none(), "{url}");
        }
        assert!(PasskeyBridge::new("http://127.0.0.1:5000/passkey", "").is_none());
    }

    #[test]
    fn the_address_override_only_ever_points_at_this_machine() {
        assert!(NatixisConfig::for_local_stub("http://127.0.0.1:5000").is_some());
        for url in ["", "https://example.com", "http://192.168.1.10:5000"] {
            assert!(NatixisConfig::for_local_stub(url).is_none(), "{url}");
        }
    }

    #[test]
    fn the_bridge_never_prints_its_key() {
        let bridge =
            PasskeyBridge::new("http://127.0.0.1:5000/passkey", "very-secret-key").expect("bridge");

        assert!(!format!("{bridge:?}").contains("very-secret-key"));
    }
}
