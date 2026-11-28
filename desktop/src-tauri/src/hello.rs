//! Windows Hello as the desktop lock's optional unlock factor.
//!
//! The backend owns the session: this module only asks Windows whether the person in front
//! of the machine consents, through `UserConsentVerifier`, the same system prompt the OS
//! sign-in uses. On success the shell trades its per-launch token for a single-use unlock
//! code and walks the main window through it; page scripts cannot do any of this, because
//! the launch token never enters JavaScript. Other platforms report no Hello and keep the
//! password: Linux has no equivalent consent prompt.

use std::sync::Mutex;

use tauri::{AppHandle, Manager, State};

/// Where the backend the shell started listens, and the per-launch token only it knows.
#[derive(Debug, Clone, Default)]
pub struct BackendTarget(std::sync::Arc<Mutex<Option<(u16, String)>>>);

impl BackendTarget {
    pub fn set(&self, port: u16, token: &str) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some((port, token.to_owned()));
    }

    /// An owned handle the backend thread can keep: `tauri::State` itself only borrows.
    pub fn shared(&self) -> Self {
        self.clone()
    }

    fn get(&self) -> Option<(u16, String)> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// Whether this machine can ask for Windows Hello right now.
#[tauri::command]
pub async fn hello_available() -> bool {
    imp::available().await
}

/// Verify Windows Hello, then unlock the desktop app through it. The main window navigates
/// on success; any failure — a closed prompt included — comes back as an error string, and
/// `"cancelled"` marks the person closing the prompt rather than a failure.
#[tauri::command]
pub async fn hello_unlock(app: AppHandle, target: State<'_, BackendTarget>) -> Result<(), String> {
    imp::verify().await?;
    let (port, token) = target
        .get()
        .ok_or_else(|| "the backend is not ready".to_owned())?;
    let code = unlock_code(port, &token).await?;
    navigate_main(&app, port, &code)
}

async fn unlock_code(port: u16, token: &str) -> Result<String, String> {
    let response = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{port}/api/auth/desktop-hello-unlock"
        ))
        .header(picsou::setup_auth::desktop::DESKTOP_TOKEN_HEADER, token)
        .send()
        .await
        .map_err(|e| format!("the backend did not answer: {e}"))?;
    if !response.status().is_success() {
        return Err("the backend refused the hello unlock".to_owned());
    }
    response
        .json::<serde_json::Value>()
        .await
        .ok()
        .and_then(|body| body.get("code")?.as_str().map(str::to_owned))
        .ok_or_else(|| "the backend answered without an unlock code".to_owned())
}

fn navigate_main(app: &AppHandle, port: u16, code: &str) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "the window is gone".to_owned())?;
    let url: tauri::Url =
        format!("http://127.0.0.1:{port}/api/auth/desktop-hello-bootstrap?token={code}")
            .parse()
            .map_err(|_| "the unlock address is invalid".to_owned())?;
    window.navigate(url).map_err(|e| e.to_string())
}

#[cfg(windows)]
mod imp {
    use windows::core::HSTRING;
    use windows::Security::Credentials::UI::{
        UserConsentVerificationResult, UserConsentVerifier, UserConsentVerifierAvailability,
    };
    use windows_sys::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

    /// Best effort: without this the first WinRT call fails with `CO_E_NOTINITIALIZED`.
    fn init_com() {
        unsafe {
            let _ = CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED as u32);
        }
    }

    pub async fn available() -> bool {
        init_com();
        UserConsentVerifier::CheckAvailabilityAsync()
            .and_then(|operation| operation.get())
            .is_ok_and(|availability| availability == UserConsentVerifierAvailability::Available)
    }

    pub async fn verify() -> Result<(), String> {
        init_com();
        let lang = crate::i18n::Lang::system();
        let availability = UserConsentVerifier::CheckAvailabilityAsync()
            .and_then(|operation| operation.get())
            .map_err(|_| crate::i18n::hello_unavailable(lang).to_owned())?;
        if availability != UserConsentVerifierAvailability::Available {
            return Err(crate::i18n::hello_unavailable(lang).to_owned());
        }
        let result = UserConsentVerifier::RequestVerificationAsync(&HSTRING::from(
            crate::i18n::hello_prompt(lang),
        ))
        .and_then(|operation| operation.get())
        .map_err(|e| format!("windows hello failed: {e}"))?;
        verify_result(result, lang)
    }

    /// Maps the consent answer: only `Verified` unlocks, `Canceled` stays silent, everything
    /// else points back at the password.
    fn verify_result(
        result: UserConsentVerificationResult,
        lang: crate::i18n::Lang,
    ) -> Result<(), String> {
        match result {
            UserConsentVerificationResult::Verified => Ok(()),
            UserConsentVerificationResult::Canceled => Err("cancelled".to_owned()),
            _ => Err(crate::i18n::hello_unavailable(lang).to_owned()),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::i18n::{self, Lang};

        /// The real availability call must return on this machine (no prompt involved):
        /// no assertion on the answer, which depends on its Hello setup.
        #[tokio::test]
        async fn availability_check_returns_on_this_machine() {
            let _ = available().await;
        }

        #[test]
        fn only_a_verified_answer_unlocks() {
            for result in [
                UserConsentVerificationResult::DeviceNotPresent,
                UserConsentVerificationResult::NotConfiguredForUser,
                UserConsentVerificationResult::DisabledByPolicy,
                UserConsentVerificationResult::DeviceBusy,
                UserConsentVerificationResult::RetriesExhausted,
            ] {
                assert_eq!(
                    verify_result(result, Lang::En),
                    Err(i18n::hello_unavailable(Lang::En).to_owned())
                );
            }
            assert_eq!(
                verify_result(UserConsentVerificationResult::Canceled, Lang::En),
                Err("cancelled".to_owned())
            );
            assert!(verify_result(UserConsentVerificationResult::Verified, Lang::En).is_ok());
        }

        #[test]
        fn the_prompt_and_fallback_speak_the_four_languages() {
            for text in [
                i18n::hello_prompt(Lang::En),
                i18n::hello_prompt(Lang::Fr),
                i18n::hello_unavailable(Lang::En),
                i18n::hello_unavailable(Lang::De),
            ] {
                assert!(!text.is_empty());
            }
            assert_ne!(i18n::hello_prompt(Lang::En), i18n::hello_prompt(Lang::Fr));
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub async fn available() -> bool {
        false
    }

    pub async fn verify() -> Result<(), String> {
        Err("windows hello is only available on Windows".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_backend_target_starts_empty_and_keeps_the_latest_start() {
        let target = BackendTarget::default();
        assert!(target.get().is_none());
        target.set(8123, "first");
        target.set(8124, "second");
        assert_eq!(target.get(), Some((8124, "second".to_owned())));
    }
}
