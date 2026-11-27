//! Passkey assertions for bank logins, signed by the operating system.
//!
//! The backend asks over the loopback bank bridge; the person confirms in a prompt (Windows' own,
//! or on Linux the shell's window for a USB security key) and the private key never leaves the
//! authenticator. The shell builds the
//! `clientDataJSON` itself, for an allow-listed relying party only, so a caller can neither choose
//! the signed origin nor sign for another site. Nothing here logs or stores a challenge, a
//! credential id or a signature.

// Only Windows and Linux drive an authenticator yet; elsewhere the route answers that none is there.
#![cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(windows)]
mod windows;

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};

/// The relying parties Picsou signs for; the signed origin is `https://<rp id>`.
const ALLOWED_RP_IDS: &[&str] = &["auth-nie.natixis.com"];

const MIN_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_CHALLENGE_BYTES: usize = 1024;
const MAX_CREDENTIAL_ID_BYTES: usize = 1023;
const MAX_CREDENTIALS: usize = 64;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssertionRequest {
    rp_id: String,
    challenge: String,
    #[serde(default)]
    allow_credentials: Vec<CredentialRef>,
    user_verification: Option<String>,
    timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialRef {
    id: String,
    #[serde(default)]
    transports: Vec<String>,
}

/// Binary fields as unpadded base64url; the caller re-encodes them as its bank expects.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssertionReply {
    credential_id: String,
    authenticator_data: String,
    client_data_json: String,
    signature: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_handle: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserVerification {
    Required,
    Preferred,
    Discouraged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Usb,
    Nfc,
    Ble,
    Internal,
    Hybrid,
}

#[derive(Debug, PartialEq, Eq)]
pub struct AllowedCredential {
    pub id: Vec<u8>,
    pub transports: Vec<Transport>,
}

/// A validated request, ready for the authenticator.
#[derive(Debug)]
pub struct Prompt {
    pub rp_id: &'static str,
    /// The exact bytes the authenticator signs the SHA-256 of.
    pub client_data_json: Vec<u8>,
    /// Empty when the bank lets the authenticator pick a discoverable credential.
    pub allow_credentials: Vec<AllowedCredential>,
    pub user_verification: UserVerification,
    pub timeout: Duration,
}

#[derive(Debug)]
pub struct Assertion {
    pub credential_id: Vec<u8>,
    pub authenticator_data: Vec<u8>,
    pub signature: Vec<u8>,
    pub user_handle: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The person closed the prompt, or no passkey on this machine matched.
    Cancelled,
    TimedOut,
    /// This system has no passkey interface.
    Unavailable,
    /// Anything else Windows reported, as its status code.
    #[cfg_attr(not(windows), allow(dead_code))]
    System(i32),
}

/// The sign-in, blocking until the person answers the prompt.
pub trait Authenticator: Send + Sync + 'static {
    fn assert(&self, prompt: &Prompt) -> Result<Assertion, Failure>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refusal {
    pub status: StatusCode,
    pub detail: &'static str,
}

impl Refusal {
    const fn new(status: StatusCode, detail: &'static str) -> Self {
        Self { status, detail }
    }
}

const INVALID: Refusal = Refusal::new(StatusCode::BAD_REQUEST, "INVALID_DATA");

/// Lets one prompt through at a time: a second one would stack a dialog over the first.
#[derive(Default)]
pub struct Gate(Arc<tokio::sync::Mutex<()>>);

impl Gate {
    pub async fn assert(
        &self,
        authenticator: Arc<dyn Authenticator>,
        request: AssertionRequest,
    ) -> Result<AssertionReply, Refusal> {
        let prompt = prepare(request)?;
        let open = self
            .0
            .clone()
            .try_lock_owned()
            .map_err(|_| Refusal::new(StatusCode::CONFLICT, "PASSKEY_BUSY"))?;
        let client_data_json = prompt.client_data_json.clone();
        // The guard moves with the OS call: a caller that gives up does not free the prompt.
        let assertion = tokio::task::spawn_blocking(move || {
            let _open = open;
            authenticator.assert(&prompt)
        })
        .await
        .map_err(|_| {
            tracing::error!("the passkey prompt thread failed");
            Refusal::new(StatusCode::INTERNAL_SERVER_ERROR, "PASSKEY_FAILED")
        })?
        .map_err(refusal)?;
        let encode = |bytes: &[u8]| URL_SAFE_NO_PAD.encode(bytes);
        Ok(AssertionReply {
            credential_id: encode(&assertion.credential_id),
            authenticator_data: encode(&assertion.authenticator_data),
            client_data_json: encode(&client_data_json),
            signature: encode(&assertion.signature),
            user_handle: assertion.user_handle.as_deref().map(encode),
        })
    }
}

fn refusal(failure: Failure) -> Refusal {
    match failure {
        Failure::Cancelled => Refusal::new(StatusCode::GONE, "PASSKEY_CANCELLED"),
        Failure::TimedOut => Refusal::new(StatusCode::REQUEST_TIMEOUT, "PASSKEY_TIMEOUT"),
        Failure::Unavailable => Refusal::new(StatusCode::NOT_IMPLEMENTED, "PASSKEY_UNAVAILABLE"),
        Failure::System(code) => {
            tracing::warn!(
                "the passkey prompt failed with status {:#010x}",
                code as u32
            );
            Refusal::new(StatusCode::INTERNAL_SERVER_ERROR, "PASSKEY_FAILED")
        }
    }
}

fn prepare(request: AssertionRequest) -> Result<Prompt, Refusal> {
    let rp_id = ALLOWED_RP_IDS
        .iter()
        .copied()
        .find(|allowed| *allowed == request.rp_id)
        .ok_or_else(|| Refusal::new(StatusCode::FORBIDDEN, "PASSKEY_RP_NOT_ALLOWED"))?;
    let challenge = decode(&request.challenge, MAX_CHALLENGE_BYTES)?;
    if request.allow_credentials.len() > MAX_CREDENTIALS {
        return Err(INVALID);
    }
    let allow_credentials = request
        .allow_credentials
        .iter()
        .map(|credential| {
            Ok(AllowedCredential {
                id: decode(&credential.id, MAX_CREDENTIAL_ID_BYTES)?,
                transports: credential
                    .transports
                    .iter()
                    .filter_map(|name| transport(name))
                    .collect(),
            })
        })
        .collect::<Result<Vec<_>, Refusal>>()?;
    // An absent or unknown requirement asks for verification, never for less.
    let user_verification = match request.user_verification.as_deref() {
        Some("preferred") => UserVerification::Preferred,
        Some("discouraged") => UserVerification::Discouraged,
        _ => UserVerification::Required,
    };
    let timeout = request
        .timeout_ms
        .map_or(MAX_TIMEOUT, Duration::from_millis)
        .clamp(MIN_TIMEOUT, MAX_TIMEOUT);
    Ok(Prompt {
        rp_id,
        client_data_json: client_data_json(rp_id, &challenge),
        allow_credentials,
        user_verification,
        timeout,
    })
}

/// The browser's serialisation of `CollectedClientData`. Both values are free of characters JSON
/// would escape: an allow-listed host and base64url.
fn client_data_json(rp_id: &str, challenge: &[u8]) -> Vec<u8> {
    format!(
        r#"{{"type":"webauthn.get","challenge":"{}","origin":"https://{rp_id}","crossOrigin":false}}"#,
        URL_SAFE_NO_PAD.encode(challenge)
    )
    .into_bytes()
}

/// Banks write binary fields in either base64 alphabet, padded or not.
fn decode(value: &str, max_bytes: usize) -> Result<Vec<u8>, Refusal> {
    if value.len() > max_bytes * 2 {
        return Err(INVALID);
    }
    let url_safe: String = value
        .trim_end_matches('=')
        .chars()
        .map(|c| match c {
            '+' => '-',
            '/' => '_',
            other => other,
        })
        .collect();
    let bytes = URL_SAFE_NO_PAD.decode(url_safe).map_err(|_| INVALID)?;
    if bytes.is_empty() || bytes.len() > max_bytes {
        return Err(INVALID);
    }
    Ok(bytes)
}

fn transport(name: &str) -> Option<Transport> {
    match name {
        "usb" => Some(Transport::Usb),
        "nfc" => Some(Transport::Nfc),
        "ble" => Some(Transport::Ble),
        "internal" => Some(Transport::Internal),
        "hybrid" => Some(Transport::Hybrid),
        _ => None,
    }
}

/// The platform's authenticator, with its prompt attached to the main window.
#[cfg(windows)]
pub fn system(app: tauri::AppHandle) -> Arc<dyn Authenticator> {
    Arc::new(windows::WindowsAuthenticator::new(app))
}

/// A USB security key, with the shell's own window as the prompt.
#[cfg(target_os = "linux")]
pub fn system(app: tauri::AppHandle) -> Arc<dyn Authenticator> {
    Arc::new(linux::SecurityKeyAuthenticator::new(app))
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn system(_app: tauri::AppHandle) -> Arc<dyn Authenticator> {
    struct NoAuthenticator;
    impl Authenticator for NoAuthenticator {
        fn assert(&self, _prompt: &Prompt) -> Result<Assertion, Failure> {
            Err(Failure::Unavailable)
        }
    }
    Arc::new(NoAuthenticator)
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::sync::Mutex;

    use serde_json::{json, Value};

    use super::*;

    /// Answers with the given user handle, or fails.
    struct Answer(Result<Option<Vec<u8>>, Failure>);

    impl Authenticator for Answer {
        fn assert(&self, _prompt: &Prompt) -> Result<Assertion, Failure> {
            self.0.clone().map(|user_handle| Assertion {
                credential_id: vec![0xfb, 0xff],
                authenticator_data: vec![1, 2, 3],
                signature: vec![4, 5],
                user_handle,
            })
        }
    }

    fn request(body: Value) -> AssertionRequest {
        serde_json::from_value(body).expect("request")
    }

    fn natixis() -> Value {
        json!({ "rpId": "auth-nie.natixis.com", "challenge": "+/8=" })
    }

    #[test]
    fn the_signed_client_data_names_the_allowed_origin_and_the_challenge() {
        let prompt = prepare(request(natixis())).expect("prompt");

        assert_eq!(
            String::from_utf8(prompt.client_data_json).unwrap(),
            r#"{"type":"webauthn.get","challenge":"-_8","origin":"https://auth-nie.natixis.com","crossOrigin":false}"#
        );
        assert_eq!(prompt.rp_id, "auth-nie.natixis.com");
    }

    /// The body the backend's `PasskeyBridge` test pins, for a relying party the shell allows.
    #[test]
    fn the_shell_reads_what_the_backend_sends() {
        let body = json!({
            "rpId": "auth-nie.natixis.com",
            "challenge": "Y2hhbGxlbmdl",
            "allowCredentials": [
                { "id": "Y3JlZA" },
                { "id": "a2V5", "transports": ["usb"] },
            ],
            "userVerification": "required",
            "timeoutMs": 60000,
        });

        let prompt = prepare(request(body)).expect("prompt");

        assert_eq!(prompt.allow_credentials[1].transports, [Transport::Usb]);
        assert_eq!(prompt.timeout, Duration::from_secs(60));
    }

    #[test]
    fn another_relying_party_is_refused() {
        for rp_id in [
            "natixis.com",
            "auth-nie.natixis.com.evil.example",
            "example.com",
        ] {
            let refused = prepare(request(json!({ "rpId": rp_id, "challenge": "AAAA" })))
                .expect_err("refused");

            assert_eq!(refused.detail, "PASSKEY_RP_NOT_ALLOWED", "{rp_id}");
        }
    }

    #[test]
    fn credentials_are_decoded_with_their_known_transports() {
        let mut body = natixis();
        body["allowCredentials"] = json!([
            { "id": "-_8", "transports": ["usb", "smart-card", "internal"] },
            { "id": "-_8" },
        ]);

        let prompt = prepare(request(body)).expect("prompt");

        let credential = |transports| AllowedCredential {
            id: vec![0xfb, 0xff],
            transports,
        };
        assert_eq!(
            prompt.allow_credentials,
            [
                credential(vec![Transport::Usb, Transport::Internal]),
                credential(vec![]),
            ]
        );
    }

    #[test]
    fn user_verification_is_required_unless_the_bank_asks_for_less() {
        for (asked, expected) in [
            (None, UserVerification::Required),
            (Some("required"), UserVerification::Required),
            (Some("surprise"), UserVerification::Required),
            (Some("preferred"), UserVerification::Preferred),
            (Some("discouraged"), UserVerification::Discouraged),
        ] {
            let mut body = natixis();
            body["userVerification"] = json!(asked);

            let prompt = prepare(request(body)).expect("prompt");

            assert_eq!(prompt.user_verification, expected, "{asked:?}");
        }
    }

    #[test]
    fn the_prompt_timeout_is_bounded() {
        for (asked, expected) in [(None, 120), (Some(1_000), 30), (Some(60_000), 60)] {
            let mut body = natixis();
            body["timeoutMs"] = json!(asked);

            let prompt = prepare(request(body)).expect("prompt");

            assert_eq!(prompt.timeout, Duration::from_secs(expected), "{asked:?}");
        }
    }

    #[test]
    fn an_empty_oversized_or_malformed_challenge_is_refused() {
        let oversized = URL_SAFE_NO_PAD.encode(vec![0u8; MAX_CHALLENGE_BYTES + 1]);
        for challenge in ["", "***", oversized.as_str()] {
            let body = json!({ "rpId": "auth-nie.natixis.com", "challenge": challenge });

            assert_eq!(prepare(request(body)).expect_err("refused"), INVALID);
        }
    }

    #[test]
    fn too_many_or_oversized_credentials_and_unknown_fields_are_refused() {
        let oversized = URL_SAFE_NO_PAD.encode(vec![0u8; MAX_CREDENTIAL_ID_BYTES + 1]);
        let too_many = vec![json!({ "id": "-_8" }); MAX_CREDENTIALS + 1];
        for (field, value) in [
            ("allowCredentials", json!(too_many)),
            ("allowCredentials", json!([{ "id": oversized }])),
            ("allowCredentials", json!([{ "id": "-_8", "type": "x" }])),
            ("origin", json!("https://example.com")),
        ] {
            let mut body = natixis();
            body[field] = value;

            let read = serde_json::from_value(body).map_err(|_| INVALID);

            assert_eq!(read.and_then(prepare).expect_err(field), INVALID);
        }
    }

    #[tokio::test]
    async fn an_assertion_comes_back_as_base64url_with_the_signed_client_data() {
        let reply = Gate::default()
            .assert(Arc::new(Answer(Ok(None))), request(natixis()))
            .await
            .expect("reply");

        let reply = serde_json::to_value(reply).unwrap();
        assert_eq!(reply["credentialId"], "-_8");
        assert_eq!(reply["authenticatorData"], "AQID");
        assert_eq!(reply["signature"], "BAU");
        assert!(reply.get("userHandle").is_none());
        let signed = URL_SAFE_NO_PAD
            .decode(reply["clientDataJson"].as_str().unwrap())
            .unwrap();
        assert_eq!(
            signed,
            prepare(request(natixis())).unwrap().client_data_json
        );
    }

    #[tokio::test]
    async fn a_user_handle_the_authenticator_names_comes_back() {
        let discoverable = Arc::new(Answer(Ok(Some(vec![6]))));

        let reply = Gate::default()
            .assert(discoverable, request(natixis()))
            .await
            .expect("reply");

        assert_eq!(serde_json::to_value(reply).unwrap()["userHandle"], "Bg");
    }

    #[tokio::test]
    async fn each_failure_has_its_own_answer() {
        for (failure, status, detail) in [
            (Failure::Cancelled, 410, "PASSKEY_CANCELLED"),
            (Failure::TimedOut, 408, "PASSKEY_TIMEOUT"),
            (Failure::Unavailable, 501, "PASSKEY_UNAVAILABLE"),
            (Failure::System(-1), 500, "PASSKEY_FAILED"),
        ] {
            let refused = Gate::default()
                .assert(Arc::new(Answer(Err(failure))), request(natixis()))
                .await
                .expect_err("refused");

            assert_eq!(refused.status.as_u16(), status, "{detail}");
            assert_eq!(refused.detail, detail);
        }
    }

    /// Holds the prompt open until the test lets it go.
    struct Held {
        opened: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }

    impl Authenticator for Held {
        fn assert(&self, _prompt: &Prompt) -> Result<Assertion, Failure> {
            self.opened.send(()).unwrap();
            self.release.lock().unwrap().recv().unwrap();
            Err(Failure::Cancelled)
        }
    }

    type Caller = tokio::task::JoinHandle<Result<AssertionReply, Refusal>>;

    /// A gate whose first caller waits on an open prompt, until the sender releases it.
    async fn open_prompt() -> (Arc<Gate>, Caller, mpsc::Sender<()>) {
        let (opened, is_open) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let gate = Arc::new(Gate::default());
        let held = Arc::new(Held {
            opened,
            release: Mutex::new(released),
        });
        let first = tokio::spawn({
            let gate = gate.clone();
            async move { gate.assert(held, request(natixis())).await }
        });
        tokio::task::spawn_blocking(move || is_open.recv().unwrap())
            .await
            .unwrap();
        (gate, first, release)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_caller_that_gives_up_does_not_free_the_prompt() {
        let (gate, first, release) = open_prompt().await;
        first.abort();
        assert!(first.await.expect_err("aborted").is_cancelled());

        let second = gate
            .assert(Arc::new(Answer(Ok(None))), request(natixis()))
            .await;

        release.send(()).unwrap();
        assert_eq!(second.expect_err("busy").detail, "PASSKEY_BUSY");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_second_prompt_is_refused_while_one_is_open() {
        let (gate, first, release) = open_prompt().await;

        let second = gate
            .assert(Arc::new(Answer(Ok(None))), request(natixis()))
            .await;

        assert_eq!(second.expect_err("busy").detail, "PASSKEY_BUSY");
        release.send(()).unwrap();
        assert_eq!(
            first.await.unwrap().expect_err("cancelled").detail,
            "PASSKEY_CANCELLED"
        );
        assert!(gate
            .assert(Arc::new(Answer(Ok(None))), request(natixis()))
            .await
            .is_ok());
    }
}
