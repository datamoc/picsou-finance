//! What the passkey window says, step by step, from what the security key reports and what the
//! person answers. No window and no USB here: both reach it as [`Event`]s.

use std::sync::mpsc::{Receiver, Sender};
use std::time::Instant;

use authenticator::ctap2::commands::{CommandError, StatusCode};
use authenticator::ctap2::server::PublicKeyCredentialUserEntity;
use authenticator::errors::{AuthenticatorError, HIDError, U2FTokenError};
use authenticator::{Pin, PinError, SignResult, StatusPinUv, StatusUpdate};
use serde::{Deserialize, Serialize};

use super::super::Failure;

/// The shortest PIN a security key accepts, in characters, and the longest, in bytes (CTAP2).
const MIN_PIN_CHARS: usize = 4;
const MAX_PIN_BYTES: usize = 63;

pub enum Event {
    Key(StatusUpdate),
    Signed(Result<SignResult, AuthenticatorError>),
    Answer(Answer),
    /// Whether a key is plugged in that this user may not open.
    LockedOut(bool),
}

/// What the person did in the window. Not `Debug`: it carries the PIN.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Answer {
    Pin(String),
    Account(usize),
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "step", rename_all = "camelCase")]
pub enum View {
    Insert,
    /// In the place of [`View::Insert`], when the key is there but closed to this user.
    NoAccess,
    SelectKey,
    Pin {
        wrong: bool,
        retries: Option<u8>,
    },
    Wait,
    Touch,
    Fingerprint {
        retries: Option<u8>,
    },
    Accounts {
        names: Vec<String>,
    },
    // The sign-in cannot go on: the window says why until the person closes it.
    NoCredential,
    PinNotSet,
    PinBlocked,
    Replug,
    Unsupported,
    Failed,
}

/// The question the key is waiting on. Dropping it tells the key the person gave up.
enum Question {
    None,
    Pin(Sender<Pin>),
    Account(Sender<Option<usize>>, usize),
}

/// Runs one sign-in until the key signs, the person gives up or `deadline` passes. It owns
/// `events`, so every reply channel still waiting is closed when it returns: the key's thread
/// never stays blocked on a PIN nobody will type.
pub fn run(
    events: Receiver<Event>,
    mut show: impl FnMut(View),
    deadline: Instant,
) -> Result<SignResult, Failure> {
    let mut question = Question::None;
    let mut shown = View::Insert;
    // Set once a final view is up: only closing the window is left to do.
    let mut over = false;
    show(shown.clone());
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let Ok(event) = events.recv_timeout(left) else {
            return Err(if over {
                Failure::Cancelled
            } else {
                Failure::TimedOut
            });
        };
        let next = match event {
            Event::Answer(Answer::Close) => return Err(Failure::Cancelled),
            _ if over => None,
            Event::Answer(Answer::Pin(pin)) => match &question {
                // A PIN the key cannot hold would still cost one of its few attempts.
                Question::Pin(reply)
                    if pin.chars().count() >= MIN_PIN_CHARS && pin.len() <= MAX_PIN_BYTES =>
                {
                    let _ = reply.send(Pin::new(&pin));
                    question = Question::None;
                    Some(View::Wait)
                }
                _ => None,
            },
            Event::Answer(Answer::Account(index)) => match &question {
                Question::Account(reply, count) if index < *count => {
                    let _ = reply.send(Some(index));
                    question = Question::None;
                    Some(View::Wait)
                }
                _ => None,
            },
            Event::Key(status) => match status {
                StatusUpdate::PresenceRequired => Some(View::Touch),
                StatusUpdate::SelectDeviceNotice => Some(View::SelectKey),
                StatusUpdate::PinUvError(StatusPinUv::PinRequired(reply)) => {
                    question = Question::Pin(reply);
                    Some(View::Pin {
                        wrong: false,
                        retries: None,
                    })
                }
                StatusUpdate::PinUvError(StatusPinUv::InvalidPin(reply, retries)) => {
                    question = Question::Pin(reply);
                    Some(View::Pin {
                        wrong: true,
                        retries,
                    })
                }
                StatusUpdate::PinUvError(StatusPinUv::InvalidUv(retries)) => {
                    Some(View::Fingerprint { retries })
                }
                StatusUpdate::SelectResultNotice(reply, users) => {
                    question = Question::Account(reply, users.len());
                    Some(View::Accounts {
                        names: users.iter().map(account_name).collect(),
                    })
                }
                // A blocked or missing PIN also ends the signature, with an error that says so;
                // reading it there does not depend on which of the two arrives first.
                _ => None,
            },
            // Only while no key has answered: one that did says more than this check.
            Event::LockedOut(locked) => match (&shown, locked) {
                (View::Insert, true) => Some(View::NoAccess),
                (View::NoAccess, false) => Some(View::Insert),
                _ => None,
            },
            Event::Signed(Ok(signed)) => return Ok(signed),
            Event::Signed(Err(error)) => {
                let view = dead_end(&error)?;
                over = true;
                question = Question::None;
                Some(view)
            }
        };
        if let Some(next) = next {
            if next != shown {
                shown = next;
                show(shown.clone());
            }
        }
    }
}

/// The view that explains `error`, or the failure when there is nothing to explain.
fn dead_end(error: &AuthenticatorError) -> Result<View, Failure> {
    Ok(match error {
        AuthenticatorError::CancelledByUser => return Err(Failure::Cancelled),
        // How the library reports that its own time ran out.
        AuthenticatorError::U2FToken(U2FTokenError::NotAllowed) => return Err(Failure::TimedOut),
        AuthenticatorError::HIDError(HIDError::Command(CommandError::StatusCode(
            StatusCode::NoCredentials,
            _,
        ))) => View::NoCredential,
        AuthenticatorError::PinError(PinError::PinNotSet) => View::PinNotSet,
        AuthenticatorError::PinError(PinError::PinBlocked) => View::PinBlocked,
        AuthenticatorError::PinError(PinError::PinAuthBlocked) => View::Replug,
        AuthenticatorError::UnsupportedOption(_) => View::Unsupported,
        other => {
            tracing::warn!("the security key sign-in failed: {other}");
            View::Failed
        }
    })
}

/// A key may keep several passkeys for the same bank; it names each when it can.
fn account_name(user: &PublicKeyCredentialUserEntity) -> String {
    user.name
        .clone()
        .or_else(|| user.display_name.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{channel, TryRecvError};
    use std::time::Duration;

    use authenticator::ctap2::attestation::{AuthenticatorData, AuthenticatorDataFlags};
    use authenticator::ctap2::server::{AuthenticatorAttachment, RpIdHash};
    use authenticator::Assertion;

    use super::*;

    fn signed() -> SignResult {
        SignResult {
            assertion: Assertion {
                credentials: None,
                auth_data: AuthenticatorData {
                    rp_id_hash: RpIdHash([1; 32]),
                    flags: AuthenticatorDataFlags::USER_PRESENT,
                    counter: 7,
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

    /// Plays `events` in order, then gives the outcome and every view shown.
    fn play(events: Vec<Event>) -> (Result<SignResult, Failure>, Vec<View>) {
        let (sender, receiver) = channel();
        for event in events {
            sender.send(event).expect("queued");
        }
        let mut views = Vec::new();
        let deadline = Instant::now() + Duration::from_millis(200);
        let outcome = run(receiver, |view| views.push(view), deadline);
        (outcome, views)
    }

    fn pin(value: &str) -> Event {
        Event::Answer(Answer::Pin(value.to_owned()))
    }

    fn wants_pin() -> (Event, Receiver<Pin>) {
        let (reply, typed) = channel();
        let status = StatusUpdate::PinUvError(StatusPinUv::PinRequired(reply));
        (Event::Key(status), typed)
    }

    #[test]
    fn a_pin_then_a_touch_end_in_the_signature() {
        let (wanted, typed) = wants_pin();

        let (outcome, views) = play(vec![
            wanted,
            pin("1234"),
            Event::Key(StatusUpdate::PresenceRequired),
            Event::Signed(Ok(signed())),
        ]);

        assert_eq!(outcome.expect("signed").assertion.signature, vec![9]);
        assert_eq!(typed.recv().expect("pin").as_bytes(), b"1234");
        assert_eq!(
            views,
            vec![
                View::Insert,
                View::Pin {
                    wrong: false,
                    retries: None
                },
                View::Wait,
                View::Touch,
            ]
        );
    }

    #[test]
    fn a_wrong_pin_is_asked_again_with_the_attempts_left() {
        let (reply, typed) = channel();
        let again = StatusUpdate::PinUvError(StatusPinUv::InvalidPin(reply, Some(2)));

        let (_, views) = play(vec![
            Event::Key(again),
            pin("5678"),
            Event::Answer(Answer::Close),
        ]);

        assert_eq!(
            views[1],
            View::Pin {
                wrong: true,
                retries: Some(2)
            }
        );
        assert_eq!(typed.recv().expect("pin").as_bytes(), b"5678");
    }

    #[test]
    fn a_pin_the_key_cannot_hold_is_not_sent() {
        let (wanted, typed) = wants_pin();
        let too_long = "1".repeat(MAX_PIN_BYTES + 1);

        let _ = play(vec![
            wanted,
            pin("123"),
            pin(&too_long),
            Event::Answer(Answer::Close),
        ]);

        // The reply channel closed without a PIN ever going through it.
        assert_eq!(
            typed.try_recv().expect_err("nothing"),
            TryRecvError::Disconnected
        );
    }

    #[test]
    fn closing_the_window_frees_the_key_waiting_for_a_pin() {
        let (wanted, typed) = wants_pin();

        let (outcome, _) = play(vec![wanted, Event::Answer(Answer::Close)]);

        assert_eq!(outcome.expect_err("cancelled"), Failure::Cancelled);
        assert!(typed.recv().is_err());
    }

    #[test]
    fn nobody_answering_ends_at_the_deadline() {
        let (wanted, typed) = wants_pin();

        let (outcome, _) = play(vec![wanted]);

        assert_eq!(outcome.expect_err("timed out"), Failure::TimedOut);
        assert!(typed.recv().is_err());
    }

    #[test]
    fn the_chosen_account_goes_back_to_the_key() {
        let (reply, chosen) = channel();
        let user = |name: &str| PublicKeyCredentialUserEntity {
            id: vec![1],
            name: Some(name.to_owned()),
            display_name: None,
        };
        let notice = StatusUpdate::SelectResultNotice(reply, vec![user("a"), user("b")]);

        let (_, views) = play(vec![
            Event::Key(notice),
            Event::Answer(Answer::Account(2)),
            Event::Answer(Answer::Account(1)),
            Event::Answer(Answer::Close),
        ]);

        assert_eq!(
            views[1],
            View::Accounts {
                names: vec!["a".to_owned(), "b".to_owned()]
            }
        );
        assert_eq!(chosen.recv().expect("choice"), Some(1));
    }

    #[test]
    fn a_key_closed_to_this_user_is_said_until_it_opens_or_leaves() {
        let (outcome, views) = play(vec![
            Event::LockedOut(false),
            Event::LockedOut(true),
            Event::LockedOut(true),
            Event::LockedOut(false),
            Event::LockedOut(true),
            Event::Key(StatusUpdate::PresenceRequired),
            // A second, closed key does not hide what the working one asks for.
            Event::LockedOut(true),
            Event::Answer(Answer::Close),
        ]);

        assert_eq!(outcome.expect_err("cancelled"), Failure::Cancelled);
        assert_eq!(
            views,
            vec![
                View::Insert,
                View::NoAccess,
                View::Insert,
                View::NoAccess,
                View::Touch,
            ]
        );
    }

    #[test]
    fn a_dead_end_stays_on_screen_until_closed() {
        let no_credential = || {
            AuthenticatorError::HIDError(HIDError::Command(CommandError::StatusCode(
                StatusCode::NoCredentials,
                None,
            )))
        };
        for (error, view) in [
            (no_credential(), View::NoCredential),
            (
                AuthenticatorError::PinError(PinError::PinNotSet),
                View::PinNotSet,
            ),
            (
                AuthenticatorError::PinError(PinError::PinBlocked),
                View::PinBlocked,
            ),
            (
                AuthenticatorError::PinError(PinError::PinAuthBlocked),
                View::Replug,
            ),
            (AuthenticatorError::Platform, View::Failed),
        ] {
            // A status arriving late does not replace the explanation.
            let (outcome, views) = play(vec![
                Event::Signed(Err(error)),
                Event::Key(StatusUpdate::PresenceRequired),
            ]);

            assert_eq!(outcome.expect_err("over"), Failure::Cancelled);
            assert_eq!(views, vec![View::Insert, view]);
        }
    }

    #[test]
    fn the_library_giving_up_is_a_cancellation_or_a_timeout() {
        for (error, failure) in [
            (AuthenticatorError::CancelledByUser, Failure::Cancelled),
            (
                AuthenticatorError::U2FToken(U2FTokenError::NotAllowed),
                Failure::TimedOut,
            ),
        ] {
            let (outcome, _) = play(vec![Event::Signed(Err(error))]);

            assert_eq!(outcome.expect_err("over"), failure);
        }
    }

    #[test]
    fn the_window_receives_the_pin_answer_in_its_wire_form() {
        let answer: Answer = serde_json::from_str(r#"{"pin":"1234"}"#).unwrap();
        assert!(matches!(answer, Answer::Pin(pin) if pin == "1234"));
        let answer: Answer = serde_json::from_str(r#""close""#).unwrap();
        assert!(matches!(answer, Answer::Close));
    }
}
