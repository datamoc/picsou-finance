//! BoursoBank's passkey ceremony. The session and assertion remain local to this attempt.
//!
//! The answer to the panel cannot say which step failed, so each refusal is logged with its step.
//! The log never holds a value the bank or the authenticator sent: statuses, sizes, plain path
//! segments, member names and JSON types only.
use super::*;
use crate::native_sidecars::natixis::passkey::{Options, ask_the_shell};

/// What the login page's own script accepts, on its form calls and on its JSON ones.
const FORM_ACCEPT: &str = "application/json, text/javascript, */*; q=0.01";
const JSON_ACCEPT: &str = "application/json";

/// The bank's own sites a signed-in page may redirect to, besides the one the session talks to.
const LANDING_HOSTS: [&str; 4] = [
    "clients.boursobank.com",
    "clients.boursorama.com",
    "api.boursobank.com",
    "www.boursobank.com",
];
const MAX_LANDING_HOPS: usize = 10;

static META_TAG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<meta\b[^>]*>").unwrap());
static CONTENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\bcontent\s*=\s*["']([^"']+)["']"#).unwrap());

fn process_id(page: &str) -> Option<String> {
    META_TAG_RE
        .find_iter(page)
        .map(|tag| tag.as_str())
        .filter(|tag| UUID_META_RE.is_match(tag))
        .find_map(|tag| CONTENT_RE.captures(tag).map(|found| found[1].to_owned()))
}

/// Where a redirect may lead: the session's own origin, or one of the bank's sites over HTTPS.
fn landing_url(base: &Url, current: &Url, target: &str) -> Option<Url> {
    if let Some(url) = common::same_origin_url(base, current, target) {
        return Some(url);
    }
    let url = current.join(target).ok()?;
    let plain = url.scheme() == "https"
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none();
    let known = url.host_str().is_some_and(|h| LANDING_HOSTS.contains(&h));
    (plain && known).then_some(url)
}

/// Opens a page of the new session, following its redirects one by one. Each hop is logged, so
/// that a refused one says where the bank wanted to go.
async fn open_signed_in(
    session: &SessionClient,
    start: &str,
) -> Result<(Url, StatusCode, String), NativeError> {
    let mut url = landing_url(&session.base_origin, &session.base_origin, start)
        .ok_or_else(|| changed("landing page", &refused_site(&session.base_origin, start)))?;
    for _ in 0..=MAX_LANDING_HOPS {
        let response = session
            .no_redirect
            .get(url.clone())
            .send()
            .await
            .map_err(http_err)?;
        let status = response.status();
        let location = response
            .headers()
            .get(http::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let Some(location) = location.filter(|_| status.is_redirection()) else {
            return Ok((url, status, read_text(response).await?));
        };
        if redirects_to_logout(status, Some(&location), url.as_str()) {
            tracing::warn!("BoursoBank passkey sign-in was sent to the logout page");
            return Err(session_expired());
        }
        let next = landing_url(&session.base_origin, &url, &location)
            .ok_or_else(|| changed("landing page", &refused_site(&url, &location)))?;
        tracing::info!(
            "BoursoBank passkey sign-in follows {} to {}{}",
            status.as_u16(),
            next.host_str().unwrap_or_default(),
            safe_path(next.path())
        );
        url = next;
    }
    Err(changed("landing page", "too many redirects"))
}

/// A refused redirect, by its scheme, host and plain path: a site's name identifies nobody.
fn refused_site(current: &Url, target: &str) -> String {
    current.join(target).map_or_else(
        |_| "a redirect that is not a URL".to_owned(),
        |url| {
            format!(
                "refused a redirect to {}://{}{}",
                url.scheme(),
                url.host_str().unwrap_or_default(),
                safe_path(url.path())
            )
        },
    )
}

/// A step that did not answer as BoursoBank's own page expects, with what can safely be said.
fn changed(step: &str, why: &str) -> NativeError {
    tracing::warn!("BoursoBank passkey sign-in stopped at the {step}: {why}");
    NativeError::new(StatusCode::BAD_GATEWAY, "UPSTREAM_FORMAT_CHANGED")
}

/// The members of a JSON answer and their types, never their values.
fn shape(value: &Value) -> String {
    let kind = |value: &Value| match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    };
    let Value::Object(members) = value else {
        return kind(value).to_owned();
    };
    let mut listed: Vec<String> = members
        .iter()
        .map(|(name, member)| {
            let name: String = name
                .chars()
                .filter(|c| c.is_ascii_alphabetic() || *c == '_')
                .take(40)
                .collect();
            format!("{name}: {}", kind(member))
        })
        .take(20)
        .collect();
    listed.sort();
    format!("{{{}}}", listed.join(", "))
}

/// A path without its query, and without any segment that could be an identifier.
fn safe_path(location: &str) -> String {
    let path = location.split(['?', '#']).next().unwrap_or_default();
    let plain = |segment: &str| {
        segment.len() <= 40
            && segment
                .chars()
                .all(|c| c.is_ascii_lowercase() || matches!(c, '-' | '_'))
    };
    path.split('/')
        .map(|segment| if plain(segment) { segment } else { "..." })
        .collect::<Vec<_>>()
        .join("/")
}

async fn json_reply(step: &str, response: reqwest::Response) -> Result<Value, NativeError> {
    let status = response.status();
    let header = |name: http::HeaderName| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
    };
    let content_type = header(http::header::CONTENT_TYPE)
        .and_then(|value| value.split(';').next())
        .unwrap_or("no content type")
        .to_owned();
    let redirect = header(http::header::LOCATION)
        .map(|location| format!(", to {}", safe_path(location)))
        .unwrap_or_default();
    let text = read_text(response).await?;
    match serde_json::from_str::<Value>(&text) {
        Ok(value) if status.is_success() => Ok(value),
        parsed => {
            let members = parsed.map_or_else(|_| "not JSON".to_owned(), |value| shape(&value));
            Err(changed(
                step,
                &format!(
                    "answered {} ({content_type}, {} bytes{redirect}) {members}",
                    status.as_u16(),
                    text.len()
                ),
            ))
        }
    }
}

impl NativeBourso {
    pub(super) async fn handle_passkey(
        &self,
        body: Option<Value>,
    ) -> Result<NativeResponse, NativeError> {
        let body = body.ok_or_else(|| NativeError::new(StatusCode::BAD_REQUEST, "INVALID_DATA"))?;
        let login = body
            .get("customerId")
            .and_then(Value::as_str)
            .filter(|s| s.len() == 8 && s.bytes().all(|c| c.is_ascii_digit()))
            .ok_or_else(|| NativeError::new(StatusCode::BAD_REQUEST, "INVALID_DATA"))?;
        let bridge =
            self.config.passkey_bridge.as_ref().ok_or_else(|| {
                NativeError::new(StatusCode::NOT_IMPLEMENTED, "UPSTREAM_UNAVAILABLE")
            })?;
        tracing::info!("BoursoBank passkey sign-in started");
        let session = SessionClient::new(&self.config)?;
        let page = self.bootstrap(&session).await?;
        let process = process_id(&page).ok_or_else(|| {
            let why = format!(
                "no process id ({} bytes, mentions uuid: {}, webauthn: {})",
                page.len(),
                page.contains("uuid"),
                page.contains("webauthn")
            );
            changed("login page", &why)
        })?;
        let origin = &session.base_url;
        let referer = format!("{origin}/connexion/");
        let response = session
            .no_redirect
            .post(session.url("/connexion/login-status"))
            .header("x-requested-with", "XMLHttpRequest")
            .header("x-process-id", &process)
            .header(http::header::ACCEPT, FORM_ACCEPT)
            .header("origin", origin)
            .header("referer", &referer)
            .form(&[("login", login), ("platformAuthenticatorAvailable", "1")])
            .send()
            .await
            .map_err(http_err)?;
        let status = json_reply("login status", response).await?;
        if status.get("webauthn_available") != Some(&Value::Bool(true)) {
            tracing::warn!(
                "BoursoBank offers no passkey for this login: {}",
                shape(&status)
            );
            return Err(NativeError::new(
                StatusCode::NOT_IMPLEMENTED,
                "MFA_TYPE_UNSUPPORTED",
            ));
        }
        let response = session
            .no_redirect
            .post(session.url("/webauthn/authentification/preparation"))
            .header(http::header::ACCEPT, JSON_ACCEPT)
            .header("origin", origin)
            .header("referer", &referer)
            .json(&json!({"login": login}))
            .send()
            .await
            .map_err(http_err)?;
        let prepared = json_reply("challenge", response).await?;
        let resource = prepared
            .get("resourceId")
            .and_then(Value::as_str)
            .filter(|id| SAFE_PATH_SEGMENT_RE.is_match(id))
            .ok_or_else(|| {
                let why = format!("no usable resource id in {}", shape(&prepared));
                changed("challenge", &why)
            })?;
        let data = prepared
            .get("data")
            .ok_or_else(|| changed("challenge", &format!("no data in {}", shape(&prepared))))?;
        if data.get("rpId").and_then(Value::as_str) != Some("boursobank.com") {
            let why = format!("another relying party in {}", shape(data));
            return Err(changed("challenge", &why));
        }
        let options: Options = serde_json::from_value(data.clone())
            .map_err(|_| changed("challenge", &format!("unexpected options {}", shape(data))))?;
        let assertion = ask_the_shell(bridge, &options).await.map_err(|err| {
            tracing::warn!(
                "BoursoBank passkey prompt failed code={}: {}",
                err.code,
                err.reason
            );
            err.into_native()
        })?;
        let answer = json!({
            "id": assertion.credential_id,
            "rawId": assertion.credential_id,
            "response": {
                "authenticatorData": assertion.authenticator_data,
                "clientDataJSON": assertion.client_data_json,
                "signature": assertion.signature,
                "userHandle": assertion.user_handle,
            },
            "type": "public-key",
            "clientExtensionResults": {},
        });
        let response = session
            .no_redirect
            .post(session.url(&format!("/webauthn/authentification/validation/{resource}")))
            .header(http::header::ACCEPT, JSON_ACCEPT)
            .header("origin", origin)
            .header("referer", &referer)
            .json(&answer)
            .send()
            .await
            .map_err(http_err)?;
        let checked = json_reply("assertion check", response).await?;
        if checked.get("verified") != Some(&Value::Bool(true)) {
            tracing::warn!(
                "BoursoBank did not verify the assertion: {}",
                shape(&checked)
            );
            return Err(NativeError::new(
                StatusCode::UNAUTHORIZED,
                "INVALID_CREDENTIALS",
            ));
        }
        let response = session
            .no_redirect
            .post(session.url("/webauthn/connexion"))
            .header("x-requested-with", "XMLHttpRequest")
            .header("x-process-id", &process)
            .header(http::header::ACCEPT, FORM_ACCEPT)
            .header("origin", origin)
            .header("referer", format!("{origin}/connexion/webauthn"))
            .form(&[
                ("login", login),
                ("resourceId", resource),
                ("org", ""),
                ("rememberMe", "0"),
                ("platformAuthenticatorAvailable", "1"),
            ])
            .send()
            .await
            .map_err(http_err)?;
        let done = json_reply("sign-in", response).await?;
        if done.get("success") != Some(&Value::Bool(true)) {
            return Err(changed("sign-in", &format!("refused {}", shape(&done))));
        }
        let landing = done
            .get("redirectUrl")
            .and_then(Value::as_str)
            .ok_or_else(|| changed("sign-in", &format!("no redirect in {}", shape(&done))))?;
        let (mut url, mut status, mut html) = open_signed_in(&session, landing).await?;
        if !html.contains(LOGGED_IN_MARKER) {
            (url, status, html) = open_signed_in(&session, "/").await?;
        }
        if !html.contains(LOGGED_IN_MARKER) {
            let why = format!(
                "no signed-in page: {}{} answered {} ({} bytes)",
                url.host_str().unwrap_or_default(),
                safe_path(url.path()),
                status.as_u16(),
                html.len()
            );
            return Err(changed("landing page", &why));
        }
        tracing::info!("BoursoBank passkey sign-in opened a session");
        Ok(NativeResponse::ok(
            json!({"sessionState": session.cookies.serialize_json()}),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_sidecars::natixis::PasskeyBridge;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    #[test]
    fn a_refused_answer_is_described_without_its_values() {
        let answer = json!({"challenge": "secret-challenge", "timeout": 60000, "data": {"a": "b"}});
        assert_eq!(
            shape(&answer),
            "{challenge: string, data: object, timeout: number}"
        );
        assert_eq!(shape(&json!("secret")), "string");
        assert_eq!(
            safe_path("/securisation/validation/Ab12cd?token=secret"),
            "/securisation/validation/..."
        );
    }

    #[test]
    fn a_signed_in_page_may_only_redirect_to_the_banks_own_sites() {
        let base = Url::parse("https://clients.boursobank.com").unwrap();
        for target in [
            "/securisation",
            "https://clients.boursorama.com/",
            "https://www.boursobank.com/x",
            "https://api.boursobank.com/x",
        ] {
            assert!(landing_url(&base, &base, target).is_some(), "{target}");
        }
        for target in [
            "http://clients.boursorama.com/",
            "https://boursobank.com.evil.example/",
            "https://evil.example/",
            "https://user@www.boursobank.com/",
            "https://www.boursobank.com:8443/",
        ] {
            assert!(landing_url(&base, &base, target).is_none(), "{target}");
        }
    }

    #[tokio::test]
    async fn a_redirect_off_the_banks_sites_is_never_followed() {
        let bank = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/securisation"))
            .respond_with(
                ResponseTemplate::new(302).insert_header("location", "https://evil.example/"),
            )
            .mount(&bank)
            .await;
        let session = SessionClient::new(&BoursoConfig::with_base_url(bank.uri())).unwrap();

        let refused = open_signed_in(&session, "/securisation").await.unwrap_err();

        assert_eq!(refused.detail, "UPSTREAM_FORMAT_CHANGED");
        assert_eq!(bank.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn signs_in_and_refuses_an_unverified_assertion() {
        for verified in [true, false] {
            let bank = MockServer::start().await;
            let shell = MockServer::start().await;
            for (method_name, route, body) in [
                ("GET", "/connexion/", json!(null)),
                (
                    "POST",
                    "/connexion/login-status",
                    json!({"webauthn_available": true}),
                ),
                (
                    "POST",
                    "/webauthn/authentification/preparation",
                    json!({"resourceId":"resource-1", "data":{"rpId":"boursobank.com", "challenge":"AQ", "timeout":60000}}),
                ),
                (
                    "POST",
                    "/webauthn/authentification/validation/resource-1",
                    json!({"verified":verified}),
                ),
                (
                    "POST",
                    "/webauthn/connexion",
                    json!({"success":true,"redirectUrl":"/securisation"}),
                ),
                ("GET", "/securisation", json!(null)),
                ("GET", "/", json!(null)),
            ] {
                let response = if route == "/securisation" {
                    ResponseTemplate::new(302).insert_header("location", "/")
                } else if route == "/connexion/" {
                    ResponseTemplate::new(200)
                        .set_body_string("<meta name=\"uuid\" content=\"process\">")
                } else if route == "/" {
                    ResponseTemplate::new(200).set_body_string("<a href=\"/se-deconnecter\">x</a>")
                } else if route.starts_with("/webauthn/authentification/validation") {
                    // Any success status is an answer, as for the page's own script.
                    ResponseTemplate::new(201).set_body_json(body)
                } else {
                    ResponseTemplate::new(200).set_body_json(body)
                };
                let accept = match route {
                    "/connexion/login-status" | "/webauthn/connexion" => Some(FORM_ACCEPT),
                    "/webauthn/authentification/preparation"
                    | "/webauthn/authentification/validation/resource-1" => Some(JSON_ACCEPT),
                    _ => None,
                };
                let mut mock = Mock::given(method(method_name)).and(path(route));
                if let Some(accept) = accept {
                    mock = mock.and(wiremock::matchers::headers(
                        "accept",
                        accept.split(", ").collect(),
                    ));
                }
                mock.respond_with(response).mount(&bank).await;
            }
            Mock::given(method("POST")).and(path("/passkey/assertion")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"credentialId":"AQ", "authenticatorData":"Ag", "clientDataJson":"Aw", "signature":"BA"}))).mount(&shell).await;
            let mut config = BoursoConfig::with_base_url(bank.uri());
            config.passkey_bridge = PasskeyBridge::new(&format!("{}/passkey", shell.uri()), "key");
            let result = NativeBourso::with_config(config)
                .call(
                    Method::POST,
                    "/passkey",
                    Some(json!({"customerId":"12345678"})),
                )
                .await;
            if verified {
                assert!(result.unwrap()["sessionState"].is_string());
            } else {
                assert_eq!(result.unwrap_err().1, "INVALID_CREDENTIALS");
                assert!(
                    !bank
                        .received_requests()
                        .await
                        .unwrap()
                        .iter()
                        .any(|r| r.url.path() == "/webauthn/connexion")
                );
            }
        }
    }
}
