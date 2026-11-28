//! Install secrets in the OS credential store instead of on disk.
//!
//! On Windows the store is Credential Manager (DPAPI user scope, whose master
//! keys are TPM-backed on machines with a TPM); on macOS the Keychain; on
//! Linux the Secret Service provider (GNOME Keyring, KWallet) with a kernel
//! keyring fallback. This module only maps the two generated install secrets
//! onto `(service, account)` entries: the `keyring` crate talks to the
//! platform stores.

/// The service name under which Picsou entries live in the platform store.
pub const SERVICE: &str = "picsou-finance";

/// How `PICSOU_OS_KEYSTORE` selects the store. `Auto` keeps the secrets files
/// as a fallback and as the migration source; `Require` never touches files
/// and fails closed when the store is unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeystoreMode {
    Off,
    Auto,
    Require,
}

#[derive(Debug)]
pub enum StoreError {
    Unavailable(String),
    Failed(String),
    InvalidMode(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Unavailable(detail) => {
                write!(f, "OS credential store is unavailable: {detail}")
            }
            StoreError::Failed(detail) => {
                write!(f, "OS credential store failed: {detail}")
            }
            StoreError::InvalidMode(value) => write!(
                f,
                "PICSOU_OS_KEYSTORE must be off, auto or require, got '{value}'"
            ),
        }
    }
}

impl std::error::Error for StoreError {}

impl KeystoreMode {
    pub fn parse(raw: Option<&str>) -> Result<Self, StoreError> {
        match raw.map(str::trim).unwrap_or("") {
            "" | "off" | "false" | "no" | "0" => Ok(Self::Off),
            "auto" => Ok(Self::Auto),
            "require" | "on" | "true" | "yes" | "1" => Ok(Self::Require),
            other => Err(StoreError::InvalidMode(other.to_owned())),
        }
    }
}

/// The platform credential store behind one entry per install secret.
pub trait SecretStore: Send + Sync {
    fn load(&self, account: &str) -> Result<Option<String>, StoreError>;
    fn save(&self, account: &str, secret: &str) -> Result<(), StoreError>;
    fn delete(&self, account: &str) -> Result<(), StoreError>;
}

/// The real platform store, through the `keyring` crate.
pub struct KeyringStore;

impl SecretStore for KeyringStore {
    fn load(&self, account: &str) -> Result<Option<String>, StoreError> {
        let entry = keyring::Entry::new(SERVICE, account)
            .map_err(|e| StoreError::Unavailable(e.to_string()))?;
        match entry.get_password() {
            Ok(value) if value.is_empty() => Err(StoreError::Failed(format!(
                "entry '{account}' holds an empty value"
            ))),
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(keyring::Error::NoStorageAccess(e)) => Err(StoreError::Unavailable(e.to_string())),
            Err(e) => Err(StoreError::Failed(e.to_string())),
        }
    }

    fn save(&self, account: &str, secret: &str) -> Result<(), StoreError> {
        let entry = keyring::Entry::new(SERVICE, account)
            .map_err(|e| StoreError::Unavailable(e.to_string()))?;
        entry
            .set_password(secret)
            .map_err(|e| StoreError::Failed(e.to_string()))
    }

    fn delete(&self, account: &str) -> Result<(), StoreError> {
        let entry = keyring::Entry::new(SERVICE, account)
            .map_err(|e| StoreError::Unavailable(e.to_string()))?;
        match entry.delete_password() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(StoreError::Failed(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_os_keystore_modes() {
        for raw in [None, Some(""), Some("off"), Some("false")] {
            assert_eq!(KeystoreMode::parse(raw).unwrap(), KeystoreMode::Off);
        }
        assert_eq!(
            KeystoreMode::parse(Some("auto")).unwrap(),
            KeystoreMode::Auto
        );
        assert_eq!(
            KeystoreMode::parse(Some("require")).unwrap(),
            KeystoreMode::Require
        );
        assert!(KeystoreMode::parse(Some("tpm")).is_err());
    }
}
