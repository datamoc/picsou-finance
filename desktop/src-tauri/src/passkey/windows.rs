//! The Windows WebAuthn interface (`webauthn.dll`), the one browsers use. Windows reserves raw USB
//! access to security keys for it, so there is no other way to reach them without elevation.

use std::ptr;
use std::time::Instant;

use tauri::{AppHandle, Manager};
use windows_sys::core::HRESULT;
use windows_sys::Win32::Foundation::{
    ERROR_CANCELLED, ERROR_TIMEOUT, NTE_NOT_FOUND, NTE_USER_CANCELLED,
};
use windows_sys::Win32::Networking::WindowsWebServices::{
    WebAuthNAuthenticatorGetAssertion, WebAuthNFreeAssertion, WebAuthNGetApiVersionNumber,
    WEBAUTHN_ASSERTION, WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS,
    WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS_VERSION_4, WEBAUTHN_CLIENT_DATA,
    WEBAUTHN_CLIENT_DATA_CURRENT_VERSION, WEBAUTHN_CREDENTIAL_EX,
    WEBAUTHN_CREDENTIAL_EX_CURRENT_VERSION, WEBAUTHN_CREDENTIAL_LIST,
    WEBAUTHN_CREDENTIAL_TYPE_PUBLIC_KEY, WEBAUTHN_CTAP_TRANSPORT_BLE,
    WEBAUTHN_CTAP_TRANSPORT_HYBRID, WEBAUTHN_CTAP_TRANSPORT_INTERNAL, WEBAUTHN_CTAP_TRANSPORT_NFC,
    WEBAUTHN_CTAP_TRANSPORT_USB, WEBAUTHN_HASH_ALGORITHM_SHA_256,
    WEBAUTHN_USER_VERIFICATION_REQUIREMENT_DISCOURAGED,
    WEBAUTHN_USER_VERIFICATION_REQUIREMENT_PREFERRED,
    WEBAUTHN_USER_VERIFICATION_REQUIREMENT_REQUIRED,
};

use super::{Assertion, Authenticator, Failure, Prompt, Transport, UserVerification};

/// No field of a real assertion comes near this; a larger length is not trusted.
const MAX_FIELD_BYTES: u32 = 64 * 1024;

pub struct WindowsAuthenticator {
    app: AppHandle,
}

impl WindowsAuthenticator {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl Authenticator for WindowsAuthenticator {
    fn assert(&self, prompt: &Prompt) -> Result<Assertion, Failure> {
        // SAFETY: takes no argument and only reads the interface's version.
        if unsafe { WebAuthNGetApiVersionNumber() } == 0 {
            return Err(Failure::Unavailable);
        }
        let window = self
            .app
            .get_webview_window("main")
            .ok_or(Failure::Unavailable)?;
        // The prompt belongs to this window: it must be visible, even when Picsou sits in the tray.
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        let hwnd = window.hwnd().map_err(|_| Failure::Unavailable)?;
        get_assertion(hwnd.0, prompt)
    }
}

fn get_assertion(hwnd: *mut core::ffi::c_void, prompt: &Prompt) -> Result<Assertion, Failure> {
    // Every buffer below outlives the call, which reads them through raw pointers.
    let rp_id: Vec<u16> = prompt.rp_id.encode_utf16().chain([0]).collect();
    let mut client_data_json = prompt.client_data_json.clone();
    let client_data = WEBAUTHN_CLIENT_DATA {
        dwVersion: WEBAUTHN_CLIENT_DATA_CURRENT_VERSION,
        cbClientDataJSON: client_data_json.len() as u32,
        pbClientDataJSON: client_data_json.as_mut_ptr(),
        pwszHashAlgId: WEBAUTHN_HASH_ALGORITHM_SHA_256,
    };

    let mut ids: Vec<Vec<u8>> = prompt
        .allow_credentials
        .iter()
        .map(|c| c.id.clone())
        .collect();
    let mut credentials: Vec<WEBAUTHN_CREDENTIAL_EX> = prompt
        .allow_credentials
        .iter()
        .zip(ids.iter_mut())
        .map(|(credential, id)| WEBAUTHN_CREDENTIAL_EX {
            dwVersion: WEBAUTHN_CREDENTIAL_EX_CURRENT_VERSION,
            cbId: id.len() as u32,
            pbId: id.as_mut_ptr(),
            pwszCredentialType: WEBAUTHN_CREDENTIAL_TYPE_PUBLIC_KEY,
            dwTransports: credential
                .transports
                .iter()
                .fold(0, |bits, transport| bits | transport_bit(*transport)),
        })
        .collect();
    let mut pointers: Vec<*mut WEBAUTHN_CREDENTIAL_EX> =
        credentials.iter_mut().map(ptr::from_mut).collect();
    let mut allow_list = WEBAUTHN_CREDENTIAL_LIST {
        cCredentials: pointers.len() as u32,
        ppCredentials: pointers.as_mut_ptr(),
    };

    // SAFETY: a structure of integers and pointers, for which all zeroes means "not set".
    let mut options: WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS = unsafe { std::mem::zeroed() };
    // Version 4 is the first with the allow list and exists since the first release of the API.
    options.dwVersion = WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS_VERSION_4;
    options.dwTimeoutMilliseconds = prompt.timeout.as_millis() as u32;
    options.dwUserVerificationRequirement = match prompt.user_verification {
        UserVerification::Required => WEBAUTHN_USER_VERIFICATION_REQUIREMENT_REQUIRED,
        UserVerification::Preferred => WEBAUTHN_USER_VERIFICATION_REQUIREMENT_PREFERRED,
        UserVerification::Discouraged => WEBAUTHN_USER_VERIFICATION_REQUIREMENT_DISCOURAGED,
    };
    if !prompt.allow_credentials.is_empty() {
        options.pAllowCredentialList = &mut allow_list;
    }

    let mut assertion: *mut WEBAUTHN_ASSERTION = ptr::null_mut();
    let started = Instant::now();
    // SAFETY: the strings are NUL-terminated, every pointer in `client_data` and `options` points
    // into a local that lives until the call returns, and `assertion` is a valid out pointer.
    let result = unsafe {
        WebAuthNAuthenticatorGetAssertion(
            hwnd,
            rp_id.as_ptr(),
            &client_data,
            &options,
            &mut assertion,
        )
    };
    if result < 0 {
        return Err(failure(result, started.elapsed() >= prompt.timeout));
    }
    if assertion.is_null() {
        return Err(Failure::System(result));
    }
    // SAFETY: a successful call returns an assertion that stays valid until it is freed, below,
    // once its fields are copied.
    let copied = unsafe { copy(&*assertion) };
    // SAFETY: `assertion` came from `WebAuthNAuthenticatorGetAssertion` and is freed once.
    unsafe { WebAuthNFreeAssertion(assertion) };
    copied.ok_or(Failure::System(result))
}

/// # Safety
/// Each pointer of `assertion` must be null or point at as many bytes as its length says.
unsafe fn copy(assertion: &WEBAUTHN_ASSERTION) -> Option<Assertion> {
    let bytes = |pointer: *const u8, length: u32| {
        if pointer.is_null() || length == 0 || length > MAX_FIELD_BYTES {
            return None;
        }
        // SAFETY: the caller guarantees `length` readable bytes at a non-null `pointer`.
        Some(unsafe { std::slice::from_raw_parts(pointer, length as usize) }.to_vec())
    };
    Some(Assertion {
        credential_id: bytes(assertion.Credential.pbId, assertion.Credential.cbId)?,
        authenticator_data: bytes(assertion.pbAuthenticatorData, assertion.cbAuthenticatorData)?,
        signature: bytes(assertion.pbSignature, assertion.cbSignature)?,
        user_handle: bytes(assertion.pbUserId, assertion.cbUserId),
    })
}

fn failure(result: HRESULT, ran_out: bool) -> Failure {
    // HRESULT_FROM_WIN32 of a Win32 error code.
    let win32 = |code: u32| (0x8007_0000 | code) as HRESULT;
    if ran_out || result == win32(ERROR_TIMEOUT) {
        Failure::TimedOut
    } else if result == NTE_USER_CANCELLED
        || result == win32(ERROR_CANCELLED)
        || result == NTE_NOT_FOUND
    {
        Failure::Cancelled
    } else {
        Failure::System(result)
    }
}

fn transport_bit(transport: Transport) -> u32 {
    match transport {
        Transport::Usb => WEBAUTHN_CTAP_TRANSPORT_USB,
        Transport::Nfc => WEBAUTHN_CTAP_TRANSPORT_NFC,
        Transport::Ble => WEBAUTHN_CTAP_TRANSPORT_BLE,
        Transport::Internal => WEBAUTHN_CTAP_TRANSPORT_INTERNAL,
        Transport::Hybrid => WEBAUTHN_CTAP_TRANSPORT_HYBRID,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_status_codes_map_to_what_the_person_did() {
        let win32 = |code: u32| (0x8007_0000 | code) as HRESULT;

        assert_eq!(failure(NTE_USER_CANCELLED, false), Failure::Cancelled);
        assert_eq!(failure(win32(1223), false), Failure::Cancelled);
        assert_eq!(failure(win32(1460), false), Failure::TimedOut);
        // Windows reports a prompt left open past its timeout as a cancellation.
        assert_eq!(failure(win32(1223), true), Failure::TimedOut);
        assert_eq!(failure(-1, false), Failure::System(-1));
    }
}
