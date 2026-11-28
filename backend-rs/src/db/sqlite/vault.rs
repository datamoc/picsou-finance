//! At-rest AES-256-GCM encrypted vault envelope for standalone SQLite databases.
//!
//! When `PICSOU_DB_ENCRYPTION=auto|require` is active, the SQLite database file is sealed into
//! `<db>.vault` on clean shutdown using the 32-byte key derived from `CRYPTO_ENCRYPTION_KEY`
//! (stored in the OS credential store on Windows/Linux), and unsealed transparently on startup.
//! Header format: `PICSOU_DBV1\0` (12 bytes) || nonce (12 bytes) || AES-256-GCM ciphertext+tag.

use std::fs;
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use rand::RngExt;
use sha2::{Digest, Sha256};

const VAULT_MAGIC: &[u8; 12] = b"PICSOU_DBV1\0";
const NONCE_LEN: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultMode {
    Off,
    Auto,
    Require,
}

impl VaultMode {
    pub fn parse(raw: Option<&str>) -> Result<Self, String> {
        match raw.map(str::trim).filter(|s| !s.is_empty()) {
            None => Ok(Self::Off),
            Some(v) if v.eq_ignore_ascii_case("off") || v.eq_ignore_ascii_case("false") => {
                Ok(Self::Off)
            }
            Some(v) if v.eq_ignore_ascii_case("auto") => Ok(Self::Auto),
            Some(v) if v.eq_ignore_ascii_case("require") || v.eq_ignore_ascii_case("true") => {
                Ok(Self::Require)
            }
            Some(other) => Err(format!(
                "Invalid PICSOU_DB_ENCRYPTION value '{other}' (expected off, auto, or require)"
            )),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("SQLite vault I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Invalid SQLite vault header")]
    InvalidHeader,
    #[error("SQLite vault encryption failed")]
    Encrypt,
    #[error("SQLite vault decryption failed (wrong key or tampered file)")]
    Decrypt,
}

/// Derives a domain-separated 256-bit AES key from Picsou's master secret.
fn derive_vault_key(master_secret: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"picsou-sqlite-vault-v1:");
    hasher.update(master_secret.as_bytes());
    let digest = hasher.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&digest);
    key
}

/// Returns the companion `.vault` path for a SQLite database file.
pub fn vault_path(db_path: &Path) -> PathBuf {
    let mut os = db_path.as_os_str().to_os_string();
    os.push(".vault");
    PathBuf::from(os)
}

/// Encrypts raw bytes into a self-authenticating `PICSOU_DBV1` envelope.
pub fn seal_bytes(plaintext: &[u8], master_secret: &str) -> Result<Vec<u8>, VaultError> {
    let key = derive_vault_key(master_secret);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| VaultError::Encrypt)?;
    let nonce_bytes: [u8; NONCE_LEN] = rand::rng().random();
    let nonce = Nonce::from(nonce_bytes);
    let ciphertext = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad: VAULT_MAGIC,
            },
        )
        .map_err(|_| VaultError::Encrypt)?;
    let mut out = Vec::with_capacity(VAULT_MAGIC.len() + NONCE_LEN + ciphertext.len());
    out.extend_from_slice(VAULT_MAGIC);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypts and authenticates a `PICSOU_DBV1` envelope.
pub fn unseal_bytes(sealed: &[u8], master_secret: &str) -> Result<Vec<u8>, VaultError> {
    if sealed.len() < VAULT_MAGIC.len() + NONCE_LEN + 16
        || &sealed[..VAULT_MAGIC.len()] != VAULT_MAGIC
    {
        return Err(VaultError::InvalidHeader);
    }
    let key = derive_vault_key(master_secret);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| VaultError::Decrypt)?;
    let nonce_start = VAULT_MAGIC.len();
    let nonce_end = nonce_start + NONCE_LEN;
    let nonce = Nonce::try_from(&sealed[nonce_start..nonce_end]).map_err(|_| VaultError::Decrypt)?;
    cipher
        .decrypt(
            &nonce,
            Payload {
                msg: &sealed[nonce_end..],
                aad: VAULT_MAGIC,
            },
        )
        .map_err(|_| VaultError::Decrypt)
}

/// Seals `db_path` atomically into `vault_path(db_path)`.
pub fn seal_sqlite_file(db_path: &Path, master_secret: &str) -> Result<PathBuf, VaultError> {
    let raw = fs::read(db_path)?;
    let sealed = seal_bytes(&raw, master_secret)?;
    let target = vault_path(db_path);
    let tmp = target.with_extension("vault.tmp");
    fs::write(&tmp, &sealed)?;
    fs::rename(&tmp, &target)?;
    Ok(target)
}

/// Unseals `vault_path(db_path)` into `db_path` when a vault file exists and `db_path` does not.
pub fn unseal_sqlite_file_if_present(
    db_path: &Path,
    master_secret: &str,
) -> Result<bool, VaultError> {
    let target = vault_path(db_path);
    if !target.exists() {
        return Ok(false);
    }
    let sealed = fs::read(&target)?;
    let plain = unseal_bytes(&sealed, master_secret)?;
    if let Some(parent) = db_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    let tmp = db_path.with_extension("db.unseal.tmp");
    fs::write(&tmp, &plain)?;
    fs::rename(&tmp, db_path)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_and_unseal_round_trip_and_reject_tampering() {
        let payload = b"SQLite format 3\0synthetic-test-payload";
        let secret = "test-master-secret-key-32-bytes!";
        let sealed = seal_bytes(payload, secret).expect("seal");
        assert_ne!(&sealed[..], payload);

        let unsealed = unseal_bytes(&sealed, secret).expect("unseal");
        assert_eq!(unsealed, payload);

        // Wrong key must fail
        assert!(matches!(
            unseal_bytes(&sealed, "wrong-secret"),
            Err(VaultError::Decrypt)
        ));

        // Tampered ciphertext must fail
        let mut tampered = sealed.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0xFF;
        assert!(matches!(
            unseal_bytes(&tampered, secret),
            Err(VaultError::Decrypt)
        ));
    }

    #[test]
    fn vault_mode_parses_all_variants() {
        assert_eq!(VaultMode::parse(None).unwrap(), VaultMode::Off);
        assert_eq!(VaultMode::parse(Some("auto")).unwrap(), VaultMode::Auto);
        assert_eq!(
            VaultMode::parse(Some("require")).unwrap(),
            VaultMode::Require
        );
        assert!(VaultMode::parse(Some("bogus")).is_err());
    }
}
