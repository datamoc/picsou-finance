//! Linux has no passkey interface, so the shell talks to a USB security key itself, over
//! `hidraw`, with Mozilla's `authenticator` library (the one Firefox uses). What Windows' own
//! prompt does is done by a small window of the shell ([`ceremony`]): the PIN is typed there and
//! goes to the key, never to the page the backend serves nor to the backend.

mod access;
mod ceremony;

use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use authenticator::authenticatorservice::{AuthenticatorService, SignArgs};
use authenticator::ctap2::server::{
    PublicKeyCredentialDescriptor, Transport as KeyTransport, UserVerificationRequirement,
};
use authenticator::statecallback::StateCallback;
use authenticator::{SignResult, StatusUpdate};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use super::{Assertion, Authenticator, Failure, Prompt, Transport, UserVerification};
use ceremony::{Answer, Event, View};

/// How often the window looks for a key this user may not open.
const ACCESS_CHECK: Duration = Duration::from_secs(1);

/// What the open window shows, and where its answers go.
struct Shown {
    label: String,
    screen: Screen,
    answers: Sender<Event>,
}

#[derive(Clone, Serialize)]
pub struct Screen {
    /// Moves with every new view, so the page redraws only then and keeps a half-typed PIN.
    seq: u32,
    #[serde(flatten)]
    view: View,
}

/// The passkey window's state; [`super::Gate`] lets one through at a time.
#[derive(Default)]
pub struct Dialog(Mutex<Option<Shown>>);

impl Dialog {
    fn lock(&self) -> MutexGuard<'_, Option<Shown>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Called by the passkey page only (`capabilities/passkey-prompt.json`), which asks a few times a
/// second.
#[tauri::command]
pub fn passkey_view(window: WebviewWindow, dialog: State<'_, Dialog>) -> Option<Screen> {
    dialog
        .lock()
        .as_ref()
        .filter(|shown| shown.label == window.label())
        .map(|shown| shown.screen.clone())
}

#[tauri::command]
pub fn passkey_answer(window: WebviewWindow, dialog: State<'_, Dialog>, answer: Answer) {
    if let Some(shown) = dialog
        .lock()
        .as_ref()
        .filter(|shown| shown.label == window.label())
    {
        let _ = shown.answers.send(Event::Answer(answer));
    }
}

/// The window of one sign-in, closed when dropped.
struct PromptWindow {
    app: AppHandle,
    window: WebviewWindow,
}

impl PromptWindow {
    fn open(app: &AppHandle, answers: Sender<Event>) -> tauri::Result<Self> {
        let label = format!("passkey-{}", uuid::Uuid::new_v4().simple());
        *app.state::<Dialog>().lock() = Some(Shown {
            label: label.clone(),
            screen: Screen {
                seq: 0,
                view: View::Insert,
            },
            answers: answers.clone(),
        });
        let window = WebviewWindowBuilder::new(app, &label, WebviewUrl::App("passkey.html".into()))
            .title("Picsou")
            .inner_size(420.0, 340.0)
            .resizable(false)
            .center()
            .always_on_top(true)
            .focused(true)
            .build()?;
        window.on_window_event(move |event| {
            if matches!(
                event,
                tauri::WindowEvent::CloseRequested { .. } | tauri::WindowEvent::Destroyed
            ) {
                let _ = answers.send(Event::Answer(Answer::Close));
            }
        });
        Ok(Self {
            app: app.clone(),
            window,
        })
    }

    fn show(&self, view: View) {
        if let Some(shown) = self.app.state::<Dialog>().lock().as_mut() {
            shown.screen = Screen {
                seq: shown.screen.seq + 1,
                view,
            };
        }
    }
}

impl Drop for PromptWindow {
    fn drop(&mut self) {
        *self.app.state::<Dialog>().lock() = None;
        let _ = self.window.destroy();
    }
}

pub struct SecurityKeyAuthenticator {
    app: AppHandle,
}

impl SecurityKeyAuthenticator {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl Authenticator for SecurityKeyAuthenticator {
    fn assert(&self, prompt: &Prompt) -> Result<Assertion, Failure> {
        let mut keys = AuthenticatorService::new().map_err(|_| Failure::Unavailable)?;
        keys.add_u2f_usb_hid_platform_transports();
        let (events, inbox) = mpsc::channel();
        let window = PromptWindow::open(&self.app, events.clone()).map_err(|e| {
            tracing::error!("the passkey window did not open: {e}");
            Failure::Unavailable
        })?;
        let (status, statuses) = mpsc::channel::<StatusUpdate>();
        let from_key = events.clone();
        // Ends when the library drops its sender, or the sign-in stops listening.
        std::thread::spawn(move || {
            for update in statuses {
                if from_key.send(Event::Key(update)).is_err() {
                    break;
                }
            }
        });
        // Ends once the sign-in stops listening.
        let from_system = events.clone();
        std::thread::spawn(move || {
            while from_system
                .send(Event::LockedOut(access::key_locked_out()))
                .is_ok()
            {
                std::thread::sleep(ACCESS_CHECK);
            }
        });
        let callback = StateCallback::new(Box::new(move |result| {
            let _ = events.send(Event::Signed(result));
        }));
        let deadline = Instant::now() + prompt.timeout;
        keys.sign(
            prompt.timeout.as_millis() as u64,
            sign_args(prompt),
            status,
            callback,
        )
        .map_err(|e| {
            tracing::error!("security keys cannot be reached: {e}");
            Failure::Unavailable
        })?;
        let outcome = ceremony::run(inbox, |view| window.show(view), deadline);
        // After `run`: it has closed the replies a key thread could still be waiting on.
        let _ = keys.cancel();
        assertion(outcome?, prompt)
    }
}

fn sign_args(prompt: &Prompt) -> SignArgs {
    SignArgs {
        client_data_hash: Sha256::digest(&prompt.client_data_json).into(),
        origin: format!("https://{}", prompt.rp_id),
        relying_party_id: prompt.rp_id.to_owned(),
        allow_list: prompt
            .allow_credentials
            .iter()
            .map(|credential| PublicKeyCredentialDescriptor {
                id: credential.id.clone(),
                transports: credential
                    .transports
                    .iter()
                    .filter_map(|transport| key_transport(*transport))
                    .collect(),
            })
            .collect(),
        user_verification_req: match prompt.user_verification {
            UserVerification::Required => UserVerificationRequirement::Required,
            UserVerification::Preferred => UserVerificationRequirement::Preferred,
            UserVerification::Discouraged => UserVerificationRequirement::Discouraged,
        },
        user_presence_req: true,
        extensions: Default::default(),
        pin: None,
        use_ctap1_fallback: false,
    }
}

/// The library has no name for a phone used across devices.
fn key_transport(transport: Transport) -> Option<KeyTransport> {
    match transport {
        Transport::Usb => Some(KeyTransport::USB),
        Transport::Nfc => Some(KeyTransport::NFC),
        Transport::Ble => Some(KeyTransport::BLE),
        Transport::Internal => Some(KeyTransport::Internal),
        Transport::Hybrid => None,
    }
}

fn assertion(signed: SignResult, prompt: &Prompt) -> Result<Assertion, Failure> {
    let signed = signed.assertion;
    // A key may leave the credential out when only one was asked for.
    let credential_id = match (signed.credentials, prompt.allow_credentials.as_slice()) {
        (Some(credential), _) => credential.id,
        (None, [only]) => only.id.clone(),
        (None, _) => {
            tracing::warn!("the security key did not name the credential it signed with");
            return Err(Failure::Unavailable);
        }
    };
    Ok(Assertion {
        credential_id,
        // The library rebuilds these bytes from their fields. No extension is asked for, so the
        // key adds none and the rebuilt bytes are the signed ones.
        authenticator_data: signed.auth_data.to_vec(),
        signature: signed.signature,
        user_handle: signed.user.map(|user| user.id),
    })
}

#[cfg(test)]
mod tests {
    use authenticator::ctap2::attestation::{AuthenticatorData, AuthenticatorDataFlags};
    use authenticator::ctap2::server::{AuthenticatorAttachment, RpIdHash};

    use super::super::AllowedCredential;
    use super::*;

    fn prompt(allow_credentials: Vec<AllowedCredential>) -> Prompt {
        Prompt {
            rp_id: "auth-nie.natixis.com",
            client_data_json: b"abc".to_vec(),
            allow_credentials,
            user_verification: UserVerification::Required,
            timeout: Duration::from_secs(60),
        }
    }

    fn allowed(id: u8) -> AllowedCredential {
        AllowedCredential {
            id: vec![id],
            transports: vec![Transport::Usb, Transport::Hybrid],
        }
    }

    fn unnamed() -> SignResult {
        SignResult {
            assertion: authenticator::Assertion {
                credentials: None,
                auth_data: AuthenticatorData {
                    rp_id_hash: RpIdHash([1; 32]),
                    flags: AuthenticatorDataFlags::USER_PRESENT
                        | AuthenticatorDataFlags::USER_VERIFIED,
                    counter: 0x0102_0304,
                    credential_data: None,
                    extensions: Default::default(),
                },
                signature: vec![9],
                user: None,
            },
            attachment: AuthenticatorAttachment::CrossPlatform,
            extensions: Default::default(),
        }
    }

    #[test]
    fn the_key_signs_the_hash_of_the_shell_client_data_for_the_allowed_site() {
        let args = sign_args(&prompt(vec![allowed(7)]));

        // SHA-256 of "abc".
        assert_eq!(args.client_data_hash[..4], [0xba, 0x78, 0x16, 0xbf]);
        assert_eq!(args.relying_party_id, "auth-nie.natixis.com");
        assert_eq!(args.origin, "https://auth-nie.natixis.com");
        assert_eq!(args.allow_list[0].id, vec![7]);
        assert_eq!(args.allow_list[0].transports, vec![KeyTransport::USB]);
        assert_eq!(
            args.user_verification_req,
            UserVerificationRequirement::Required
        );
        assert!(args.user_presence_req && !args.use_ctap1_fallback && args.pin.is_none());
    }

    #[test]
    fn the_authenticator_data_goes_out_as_the_key_signed_it() {
        let reply = assertion(unnamed(), &prompt(vec![allowed(7)])).expect("assertion");

        let mut expected = vec![1; 32];
        expected.extend([0x05, 1, 2, 3, 4]);
        assert_eq!(reply.authenticator_data, expected);
        assert_eq!(reply.credential_id, vec![7]);
    }

    #[test]
    fn an_unnamed_credential_is_only_guessed_when_one_was_asked_for() {
        for asked in [vec![], vec![allowed(1), allowed(2)]] {
            assert!(assertion(unnamed(), &prompt(asked)).is_err());
        }
    }

    #[test]
    fn the_passkey_page_says_every_step_in_the_four_languages() {
        let page = include_str!("../../../../ui/passkey.html");
        // Every `View`, as it is serialised, and the words around them.
        let keys = [
            "insert",
            "noAccess",
            "selectKey",
            "pin",
            "wait",
            "touch",
            "fingerprint",
            "accounts",
            "noCredential",
            "pinNotSet",
            "pinBlocked",
            "replug",
            "unsupported",
            "failed",
            "pinWrong",
            "retries",
            "pinLabel",
            "account",
            "submit",
            "cancel",
            "close",
        ];
        for key in keys {
            let entries = page
                .lines()
                .filter(|line| line.trim_start().starts_with(&format!("{key}: \"")))
                .count();
            assert_eq!(entries, 4, "{key} should be translated once per language");
        }
    }
}
