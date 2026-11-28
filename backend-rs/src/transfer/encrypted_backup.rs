//! Password-protected encrypted backups (`.picsou-backup`) and storage security status.
//!
//! Format (`PICSOU_ENC_V1`):
//! - Magic: `b"PICSOU_ENC_V1\0\0\0"` (16 bytes)
//! - PBKDF2-HMAC-SHA256 iterations: u32 big-endian (4 bytes, default 210_000)
//! - Salt: 16 random bytes
//! - Nonce: 12 random bytes
//! - AES-256-GCM ciphertext + 16-byte authentication tag (AAD = magic || iterations || salt)
//!
//! Plaintext inside AES-GCM is a gzip-compressed binary payload (SQLite database snapshot created
//! atomically with `VACUUM INTO`).

use std::io::{Read, Write};
use std::path::PathBuf;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use http::{StatusCode, header};
use pbkdf2::pbkdf2_hmac;
use rand::RngExt;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::AppState;
use crate::auth::{Auth, Role};
use crate::db::sqlite::vault;
use crate::error::{AppError, AppResult};
use crate::http::json::{Errors, Json, Validate};
use crate::routes::Routes;
use crate::transfer::upload::MultipartForm;

const MAGIC: &[u8; 16] = b"PICSOU_ENC_V1\0\0\0";
const DEFAULT_PBKDF2_ITERS: u32 = 210_000;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const HEADER_LEN: usize = 16 + 4 + SALT_LEN + NONCE_LEN;
const MIN_PASSPHRASE_LEN: usize = 8;
const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";

pub fn register(routes: &mut Routes) {
    routes
        .get("/api/backup/storage-security", storage_security)
        .post("/api/backup/encrypted/export", export_encrypted_backup)
        .post("/api/backup/encrypted/verify", verify_encrypted_backup)
        .post("/api/backup/encrypted/restore", restore_encrypted_backup);
}

#[derive(Debug, thiserror::Error)]
pub enum BackupCryptoError {
    #[error("Passphrase must be at least {MIN_PASSPHRASE_LEN} characters long")]
    WeakPassphrase,
    #[error("Backup archive is corrupted or not a valid Picsou encrypted backup")]
    InvalidFormat,
    #[error("Wrong passphrase or tampered backup archive")]
    DecryptionFailed,
    #[error("Compression error: {0}")]
    Compression(String),
}

pub fn encrypt_backup_bytes(
    plaintext: &[u8],
    passphrase: &str,
) -> Result<Vec<u8>, BackupCryptoError> {
    encrypt_backup_bytes_with_iters(plaintext, passphrase, DEFAULT_PBKDF2_ITERS)
}

fn encrypt_backup_bytes_with_iters(
    plaintext: &[u8],
    passphrase: &str,
    iterations: u32,
) -> Result<Vec<u8>, BackupCryptoError> {
    if passphrase.chars().count() < MIN_PASSPHRASE_LEN {
        return Err(BackupCryptoError::WeakPassphrase);
    }

    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(plaintext)
        .map_err(|e| BackupCryptoError::Compression(e.to_string()))?;
    let compressed = encoder
        .finish()
        .map_err(|e| BackupCryptoError::Compression(e.to_string()))?;

    let salt: [u8; SALT_LEN] = rand::rng().random();
    let nonce_bytes: [u8; NONCE_LEN] = rand::rng().random();

    let mut key = [0u8; 32];
    pbkdf2_hmac::<Sha256>(passphrase.as_bytes(), &salt, iterations, &mut key);

    let mut aad = Vec::with_capacity(16 + 4 + SALT_LEN);
    aad.extend_from_slice(MAGIC);
    aad.extend_from_slice(&iterations.to_be_bytes());
    aad.extend_from_slice(&salt);

    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| BackupCryptoError::InvalidFormat)?;
    let nonce = Nonce::from(nonce_bytes);
    let ciphertext = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: &compressed,
                aad: &aad,
            },
        )
        .map_err(|_| BackupCryptoError::InvalidFormat)?;

    let mut out = Vec::with_capacity(HEADER_LEN + ciphertext.len());
    out.extend_from_slice(&aad);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

pub fn decrypt_backup_bytes(
    archive: &[u8],
    passphrase: &str,
) -> Result<Vec<u8>, BackupCryptoError> {
    if archive.len() < HEADER_LEN + 16 || &archive[..16] != MAGIC {
        return Err(BackupCryptoError::InvalidFormat);
    }
    let iterations = u32::from_be_bytes([archive[16], archive[17], archive[18], archive[19]]);
    if iterations < 1_000 || iterations > 5_000_000 {
        return Err(BackupCryptoError::InvalidFormat);
    }
    let salt = &archive[20..20 + SALT_LEN];
    let aad = &archive[..20 + SALT_LEN];
    let nonce_bytes = &archive[20 + SALT_LEN..HEADER_LEN];
    let ciphertext = &archive[HEADER_LEN..];

    let mut key = [0u8; 32];
    pbkdf2_hmac::<Sha256>(passphrase.as_bytes(), salt, iterations, &mut key);

    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| BackupCryptoError::DecryptionFailed)?;
    let nonce = Nonce::try_from(nonce_bytes).map_err(|_| BackupCryptoError::InvalidFormat)?;
    let compressed = cipher
        .decrypt(
            &nonce,
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| BackupCryptoError::DecryptionFailed)?;

    let mut decoder = GzDecoder::new(&compressed[..]);
    let mut plaintext = Vec::new();
    decoder
        .read_to_end(&mut plaintext)
        .map_err(|e| BackupCryptoError::Compression(e.to_string()))?;
    Ok(plaintext)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StorageSecurityResponse {
    engine: &'static str,
    encrypted_backup_supported: bool,
    vault_mode: String,
    vault_present: bool,
    os_keystore_mode: String,
    database_path: Option<String>,
}

async fn storage_security(
    State(state): State<AppState>,
    _auth: Auth,
) -> AppResult<axum::Json<StorageSecurityResponse>> {
    let vault_mode = std::env::var("PICSOU_DB_ENCRYPTION").unwrap_or_else(|_| "off".to_owned());
    let os_keystore_mode =
        std::env::var("PICSOU_OS_KEYSTORE").unwrap_or_else(|_| "off".to_owned());
    let (engine, db_path, vault_present) = match &state.db {
        crate::db::Db::Sqlite(sq) => {
            let path = sq.file_path().map(std::path::Path::to_path_buf);
            let has_vault = path.as_ref().is_some_and(|p| vault::vault_path(p).exists());
            (
                "sqlite",
                path.map(|p| p.to_string_lossy().into_owned()),
                has_vault,
            )
        }
        crate::db::Db::Postgres(_) => ("postgresql", None, false),
    };
    Ok(axum::Json(StorageSecurityResponse {
        engine,
        encrypted_backup_supported: state.db.is_sqlite(),
        vault_mode,
        vault_present,
        os_keystore_mode,
        database_path: db_path,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportBackupRequest {
    passphrase: String,
}

impl Validate for ExportBackupRequest {
    fn validate(&self, errors: &mut Errors) {
        if self.passphrase.chars().count() < MIN_PASSPHRASE_LEN {
            errors.add(
                "passphrase",
                format!("Passphrase must be at least {MIN_PASSPHRASE_LEN} characters long"),
            );
        }
    }
}

async fn snapshot_sqlite_bytes(state: &AppState) -> AppResult<Vec<u8>> {
    let Some(sq) = state.db.as_sqlite() else {
        return Err(AppError::BadRequest(
            "Encrypted SQLite backups are only available in standalone SQLite mode.".to_owned(),
        ));
    };

    let tmp_path = std::env::temp_dir().join(format!(
        "picsou-backup-{}.db",
        uuid::Uuid::new_v4()
    ));
    let tmp_for_sql = tmp_path.clone();

    sq.interact(move |conn| {
        let escaped = tmp_for_sql
            .to_string_lossy()
            .replace('\'', "''");
        conn.execute(&format!("VACUUM INTO '{escaped}'"), [])?;
        Ok(())
    })
    .await
    .map_err(|e| AppError::internal(format!("Failed to snapshot SQLite database: {e}")))?;

    let bytes = std::fs::read(&tmp_path);
    let _ = std::fs::remove_file(&tmp_path);
    bytes.map_err(|e| AppError::internal(format!("Failed to read SQLite snapshot: {e}")))
}

async fn export_encrypted_backup(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<ExportBackupRequest>,
) -> AppResult<Response> {
    if auth.user.role != Role::Admin {
        return Err(AppError::Forbidden("Admin access required".to_owned()));
    }
    let snapshot = snapshot_sqlite_bytes(&state).await?;
    let passphrase = req.passphrase;
    let encrypted = tokio::task::spawn_blocking(move || encrypt_backup_bytes(&snapshot, &passphrase))
        .await
        .map_err(|e| AppError::internal(format!("Backup encryption task failed: {e}")))?
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    let ts = state.clock.now().format("%Y%m%d-%H%M%S");
    let filename = format!("picsou-backup-{ts}.picsou-backup");
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        encrypted,
    )
        .into_response())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BackupInspection {
    valid: bool,
    uncompressed_bytes: usize,
    integrity_ok: bool,
    table_count: usize,
    migration_count: usize,
}

fn inspect_sqlite_bytes(bytes: &[u8]) -> Result<BackupInspection, AppError> {
    if bytes.len() < SQLITE_HEADER.len() || &bytes[..16] != SQLITE_HEADER {
        return Err(AppError::BadRequest(
            "Decrypted payload is not a valid SQLite database.".to_owned(),
        ));
    }
    let tmp = std::env::temp_dir().join(format!("picsou-verify-{}.db", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, bytes)
        .map_err(|e| AppError::internal(format!("Failed to stage verification file: {e}")))?;
    let res = (|| -> Result<BackupInspection, AppError> {
        let conn = rusqlite::Connection::open_with_flags(
            &tmp,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|e| AppError::BadRequest(format!("Backup database could not be opened: {e}")))?;
        let integrity: String = conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap_or_else(|_| "error".to_owned());
        let table_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let migration_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM picsou_sqlite_migrations", [], |r| r.get(0))
            .unwrap_or(0);
        Ok(BackupInspection {
            valid: integrity == "ok",
            uncompressed_bytes: bytes.len(),
            integrity_ok: integrity == "ok",
            table_count: usize::try_from(table_count).unwrap_or(0),
            migration_count: usize::try_from(migration_count).unwrap_or(0),
        })
    })();
    let _ = std::fs::remove_file(&tmp);
    res
}

async fn verify_encrypted_backup(
    auth: Auth,
    form: MultipartForm,
) -> AppResult<axum::Json<BackupInspection>> {
    if auth.user.role != Role::Admin {
        return Err(AppError::Forbidden("Admin access required".to_owned()));
    }
    let file = form.file("file")?;
    let passphrase = form
        .param("passphrase")
        .filter(|p| !p.is_empty())
        .ok_or_else(|| AppError::BadRequest("Passphrase is required.".to_owned()))?
        .to_owned();
    let archive = file.bytes.to_vec();

    let inspection = tokio::task::spawn_blocking(move || {
        let plain = decrypt_backup_bytes(&archive, &passphrase)
            .map_err(|e| AppError::BadRequest(e.to_string()))?;
        inspect_sqlite_bytes(&plain)
    })
    .await
    .map_err(|e| AppError::internal(format!("Verification task failed: {e}")))??;

    Ok(axum::Json(inspection))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RestoreBackupResponse {
    staged: bool,
    restart_required: bool,
    staged_path: String,
    inspection: BackupInspection,
}

async fn restore_encrypted_backup(
    State(state): State<AppState>,
    auth: Auth,
    form: MultipartForm,
) -> AppResult<(StatusCode, axum::Json<RestoreBackupResponse>)> {
    if auth.user.role != Role::Admin {
        return Err(AppError::Forbidden("Admin access required".to_owned()));
    }
    let Some(sq) = state.db.as_sqlite() else {
        return Err(AppError::BadRequest(
            "Encrypted SQLite restore is only available in standalone SQLite mode.".to_owned(),
        ));
    };
    let db_path: PathBuf = sq
        .file_path()
        .ok_or_else(|| {
            AppError::BadRequest("Cannot restore over an in-memory SQLite database.".to_owned())
        })?
        .to_path_buf();

    let file = form.file("file")?;
    let passphrase = form
        .param("passphrase")
        .filter(|p| !p.is_empty())
        .ok_or_else(|| AppError::BadRequest("Passphrase is required.".to_owned()))?
        .to_owned();
    let archive = file.bytes.to_vec();

    let (plain, inspection) = tokio::task::spawn_blocking(move || {
        let plain = decrypt_backup_bytes(&archive, &passphrase)
            .map_err(|e| AppError::BadRequest(e.to_string()))?;
        let inspection = inspect_sqlite_bytes(&plain)?;
        if !inspection.valid {
            return Err(AppError::BadRequest(
                "Backup failed SQLite integrity_check; restore aborted.".to_owned(),
            ));
        }
        Ok::<_, AppError>((plain, inspection))
    })
    .await
    .map_err(|e| AppError::internal(format!("Restore decryption task failed: {e}")))??;

    // Stage next to the live DB as `<db>.restore-pending`; the desktop supervisor swaps it in
    // atomically before opening SQLite on next launch (taking a safety copy of the current DB).
    let mut pending_os = db_path.as_os_str().to_os_string();
    pending_os.push(".restore-pending");
    let pending_path = PathBuf::from(pending_os);
    std::fs::write(&pending_path, &plain)
        .map_err(|e| AppError::internal(format!("Failed to write staged backup: {e}")))?;

    Ok((
        StatusCode::ACCEPTED,
        axum::Json(RestoreBackupResponse {
            staged: true,
            restart_required: true,
            staged_path: pending_path.to_string_lossy().into_owned(),
            inspection,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_round_trip_and_wrong_passphrase_fails() {
        let sample = b"SQLite format 3\0hello-picsou-encrypted-backup-payload";
        let archive = encrypt_backup_bytes_with_iters(sample, "correct-horse-battery", 1_000)
            .expect("encrypt");
        assert_eq!(&archive[..16], MAGIC);

        let restored = decrypt_backup_bytes(&archive, "correct-horse-battery").expect("decrypt");
        assert_eq!(restored, sample);

        assert!(matches!(
            decrypt_backup_bytes(&archive, "wrong-passphrase"),
            Err(BackupCryptoError::DecryptionFailed)
        ));
    }

    #[test]
    fn weak_passphrase_is_rejected() {
        assert!(matches!(
            encrypt_backup_bytes_with_iters(b"abc", "short", 1_000),
            Err(BackupCryptoError::WeakPassphrase)
        ));
    }
}
