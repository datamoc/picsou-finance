//! SecretBootstrap: resolves JWT_SECRET, CRYPTO_ENCRYPTION_KEY and the datasource password,
//! generating and persisting the app-owned secrets under PICSOU_SECRETS_DIR.
//!
//! With `PICSOU_OS_KEYSTORE=auto|require`, the two generated secrets live in the OS
//! credential store instead (see `crypto::keystore`): `auto` keeps the secrets files as a
//! fallback and migrates them into the store on first boot, `require` never touches files
//! and fails closed when the store is unavailable.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rand::RngExt;

use super::keystore::{KeyringStore, KeystoreMode, SecretStore};
use crate::config::Env;

const DEFAULT_DATASOURCE_PASSWORD: &str = "picsou";
const LOCK_WAIT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Default)]
pub struct Resolved {
    pub dir: Option<PathBuf>,
    pub jwt_secret: Option<String>,
    pub crypto_key: Option<String>,
    pub datasource_password: String,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct SecretError(String);

struct Secret {
    variable: &'static str,
    file: &'static str,
    /// Size of a generated value in bytes; 0 for a secret the app never generates.
    generated_bytes: usize,
}

const JWT: Secret = Secret {
    variable: "JWT_SECRET",
    file: "jwt_secret",
    generated_bytes: 48,
};
const CRYPTO: Secret = Secret {
    variable: "CRYPTO_ENCRYPTION_KEY",
    file: "crypto_key",
    generated_bytes: 32,
};
const POSTGRES: Secret = Secret {
    variable: "POSTGRES_PASSWORD",
    file: "postgres_password",
    generated_bytes: 0,
};

/// Selects the OS credential store (`off`, `auto`, `require`); see `crypto::keystore`.
pub const OS_KEYSTORE_VARIABLE: &str = "PICSOU_OS_KEYSTORE";

fn non_empty<'a>(env: &'a Env, key: &str) -> Option<&'a str> {
    env.raw(key).filter(|v| !v.is_empty())
}

pub fn bootstrap(env: &Env) -> Result<Resolved, SecretError> {
    let mode = KeystoreMode::parse(env.raw(OS_KEYSTORE_VARIABLE))
        .map_err(|e| SecretError(e.to_string()))?;
    bootstrap_with(env, mode, &KeyringStore)
}

pub(crate) fn bootstrap_with(
    env: &Env,
    mode: KeystoreMode,
    store: &dyn SecretStore,
) -> Result<Resolved, SecretError> {
    let dir = non_empty(env, "PICSOU_SECRETS_DIR").map(PathBuf::from);
    let mut resolved = Resolved {
        dir: dir.clone(),
        ..Resolved::default()
    };
    let mut postgres = non_empty(env, POSTGRES.variable).map(str::to_owned);
    match &dir {
        Some(dir) => {
            prepare_directory(dir)?;
            resolved.jwt_secret = Some(resolve_generated(env, dir, &JWT, mode, store)?);
            resolved.crypto_key = Some(resolve_generated(env, dir, &CRYPTO, mode, store)?);
            if postgres.is_none() && non_empty(env, "SPRING_DATASOURCE_PASSWORD").is_none() {
                let file = dir.join(POSTGRES.file);
                if file.exists() {
                    postgres = Some(read(&file, POSTGRES.variable)?);
                }
            }
        }
        None => {
            resolved.jwt_secret = resolve_dirless(env, &JWT, mode, store)?;
            resolved.crypto_key = resolve_dirless(env, &CRYPTO, mode, store)?;
        }
    }
    resolved.datasource_password = non_empty(env, "SPRING_DATASOURCE_PASSWORD")
        .map(str::to_owned)
        .or(postgres)
        .unwrap_or_else(|| DEFAULT_DATASOURCE_PASSWORD.to_owned());
    Ok(resolved)
}

fn resolve_generated(
    env: &Env,
    dir: &Path,
    secret: &Secret,
    mode: KeystoreMode,
    store: &dyn SecretStore,
) -> Result<String, SecretError> {
    let file = dir.join(secret.file);
    if let Some(supplied) = non_empty(env, secret.variable) {
        if supplied.ends_with('\n') {
            // Used verbatim while the variable is set, but read back without the newline once it
            // is removed: the key would change then.
            tracing::warn!(
                "{} ends with a newline. It is used as is, but the copy in {} is read without it if the variable is removed later. Remove the newline.",
                secret.variable,
                file.display()
            );
        }
        if mode == KeystoreMode::Require {
            if file.exists() {
                tracing::warn!(
                    "ignoring stale plaintext {}: the OS credential store is authoritative in require mode",
                    file.display()
                );
            }
        } else {
            write_if_absent(&file, supplied)?;
        }
        return Ok(supplied.to_owned());
    }
    if mode != KeystoreMode::Off
        && let Some(stored) = load_stored(secret, Some(&file), mode, store)?
    {
        return Ok(stored);
    }
    if !file.exists() {
        let generated = generated(secret);
        if write_if_absent(&file, &generated)? {
            tracing::info!("generated {} at {}", secret.variable, file.display());
            return Ok(generated);
        }
    }
    read(&file, secret.variable)
}

/// A fresh random value for a generated secret.
fn generated(secret: &Secret) -> String {
    let mut bytes = vec![0u8; secret.generated_bytes];
    rand::rng().fill(&mut bytes[..]);
    STANDARD.encode(bytes)
}

/// Reads a generated secret from the OS credential store, migrating the secrets file
/// into it on first hit or generating a fresh value when neither exists.
///
/// `file` is `None` when no secrets directory is configured: the store is then the only
/// persistence. Returns `Ok(None)` in `Auto` mode when the store is unusable, so the
/// caller falls back to the secrets files; `Require` fails closed instead. A value the
/// store accepted but does not read back is always an error: two competing values for
/// one key would silently re-encrypt data under the wrong one.
fn load_stored(
    secret: &Secret,
    file: Option<&Path>,
    mode: KeystoreMode,
    store: &dyn SecretStore,
) -> Result<Option<String>, SecretError> {
    let fail = |e: super::keystore::StoreError| SecretError(format!("{}: {e}", secret.variable));
    match store.load(secret.file) {
        Ok(Some(stored)) => {
            if file.is_some_and(|f| f.exists()) {
                tracing::warn!(
                    "ignoring stale plaintext {}: the OS credential store holds {}",
                    file.map(|f| f.display().to_string()).unwrap_or_default(),
                    secret.variable
                );
            }
            Ok(Some(stored))
        }
        Ok(None) => {
            let plain = match file {
                Some(path) if path.exists() => read(path, secret.variable)?,
                _ => generated(secret),
            };
            if let Err(e) = store.save(secret.file, &plain) {
                if mode == KeystoreMode::Require {
                    return Err(fail(e));
                }
                tracing::warn!(
                    "OS credential store unavailable, keeping {} in its secrets file: {e}",
                    secret.variable
                );
                return Ok(None);
            }
            if store.load(secret.file).ok().flatten() != Some(plain.clone()) {
                return Err(SecretError(format!(
                    "{} was saved to the OS credential store but cannot be read back: refusing to start with two competing values",
                    secret.variable
                )));
            }
            match file {
                Some(path) if path.exists() => {
                    fs::remove_file(path).map_err(|_| {
                        SecretError(format!(
                            "Cannot remove migrated secret file {} for {}",
                            path.display(),
                            secret.variable
                        ))
                    })?;
                    tracing::info!("moved {} into the OS credential store", secret.variable);
                }
                _ => tracing::info!("generated {} in the OS credential store", secret.variable),
            }
            Ok(Some(plain))
        }
        Err(e) => {
            if mode == KeystoreMode::Require {
                return Err(fail(e));
            }
            tracing::warn!("OS credential store unavailable, falling back to secrets files: {e}");
            Ok(None)
        }
    }
}

/// Resolves a generated secret with no secrets directory: the environment wins, otherwise
/// the OS credential store when enabled.
fn resolve_dirless(
    env: &Env,
    secret: &Secret,
    mode: KeystoreMode,
    store: &dyn SecretStore,
) -> Result<Option<String>, SecretError> {
    if mode == KeystoreMode::Off {
        return Ok(env.raw(secret.variable).map(str::to_owned));
    }
    if let Some(supplied) = non_empty(env, secret.variable) {
        return Ok(Some(supplied.to_owned()));
    }
    load_stored(secret, None, mode, store)
}

fn prepare_directory(dir: &Path) -> Result<(), SecretError> {
    fs::create_dir_all(dir)
        .map_err(|_| SecretError(format!("Cannot create secrets directory {}", dir.display())))?;
    try_chmod(dir, 0o700);
    Ok(())
}

fn read(file: &Path, variable: &str) -> Result<String, SecretError> {
    let content = fs::read_to_string(file).map_err(|_| {
        SecretError(format!(
            "Cannot read secret file {} for {variable}",
            file.display()
        ))
    })?;
    let trimmed = content.trim_end_matches('\n');
    if trimmed.is_empty() {
        return Err(SecretError(format!(
            "Secret file {} is empty: refusing to start with a blank {variable}",
            file.display()
        )));
    }
    Ok(trimmed.to_owned())
}

/// Writes the value unless the file exists, never replacing one: a synced temp file is
/// hard-linked into place, or renamed under a lock file where hard links are unsupported.
fn write_if_absent(file: &Path, value: &str) -> Result<bool, SecretError> {
    if file.exists() {
        return Ok(false);
    }
    let fail = |_| SecretError(format!("Cannot write secret file {}", file.display()));
    let parent = file.parent().unwrap_or(Path::new("."));
    let suffix: u64 = rand::rng().random();
    let temp = parent.join(format!(".secret-{suffix:016x}.tmp"));
    let outcome = (|| -> Result<bool, SecretError> {
        {
            let mut handle = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
                .map_err(fail)?;
            try_chmod(&temp, 0o600);
            handle.write_all(value.as_bytes()).map_err(fail)?;
            handle.sync_all().map_err(fail)?;
        }
        match fs::hard_link(&temp, file) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
            Err(_) => publish_under_lock(file, &temp),
        }
    })();
    let _ = fs::remove_file(&temp);
    outcome
}

fn publish_under_lock(file: &Path, temp: &Path) -> Result<bool, SecretError> {
    let mut lock_name = file.file_name().unwrap_or_default().to_os_string();
    lock_name.push(".lock");
    let lock = file.with_file_name(lock_name);
    let deadline = Instant::now() + LOCK_WAIT;
    loop {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
        {
            Ok(_) => break,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => {
                return Err(SecretError(format!(
                    "Cannot write secret file {}",
                    file.display()
                )));
            }
        }
        if file.symlink_metadata().is_ok() {
            return Ok(false);
        }
        if Instant::now() >= deadline {
            return Err(SecretError(format!(
                "Secret file {} is still being created behind {}. Delete {} if no other Picsou instance is starting.",
                file.display(),
                lock.display(),
                lock.display()
            )));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let result = if file.symlink_metadata().is_ok() {
        Ok(false)
    } else {
        fs::rename(temp, file)
            .map(|()| true)
            .map_err(|_| SecretError(format!("Cannot write secret file {}", file.display())))
    };
    let _ = fs::remove_file(&lock);
    result
}

fn try_chmod(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::set_permissions(path, fs::Permissions::from_mode(mode)).is_err() {
            tracing::warn!(
                "could not chmod {:o} {}: the filesystem may not support it. Secrets may be readable by other users on the host.",
                mode,
                path.display()
            );
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::super::keystore::StoreError;
    use crate::config::Env;

    /// An in-memory stand-in for the platform store.
    struct MemStore {
        values: Mutex<HashMap<String, String>>,
    }

    impl MemStore {
        fn with(values: &[(&str, &str)]) -> Self {
            Self {
                values: Mutex::new(
                    values
                        .iter()
                        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                        .collect(),
                ),
            }
        }

        fn get(&self, account: &str) -> Option<String> {
            self.values
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(account)
                .cloned()
        }
    }

    impl SecretStore for MemStore {
        fn load(&self, account: &str) -> Result<Option<String>, StoreError> {
            Ok(self.get(account))
        }

        fn save(&self, account: &str, secret: &str) -> Result<(), StoreError> {
            self.values
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(account.to_owned(), secret.to_owned());
            Ok(())
        }

        fn delete(&self, _account: &str) -> Result<(), StoreError> {
            Ok(())
        }
    }

    /// A store that is never there: no D-Bus, a locked vault, a missing daemon.
    struct FailStore;

    impl SecretStore for FailStore {
        fn load(&self, _account: &str) -> Result<Option<String>, StoreError> {
            Err(StoreError::Unavailable("no store in tests".into()))
        }

        fn save(&self, _account: &str, _secret: &str) -> Result<(), StoreError> {
            Err(StoreError::Unavailable("no store in tests".into()))
        }

        fn delete(&self, _account: &str) -> Result<(), StoreError> {
            Err(StoreError::Unavailable("no store in tests".into()))
        }
    }

    /// A store that accepts a save but never reads it back.
    struct AmnesiacStore;

    impl SecretStore for AmnesiacStore {
        fn load(&self, _account: &str) -> Result<Option<String>, StoreError> {
            Ok(None)
        }

        fn save(&self, _account: &str, _secret: &str) -> Result<(), StoreError> {
            Ok(())
        }

        fn delete(&self, _account: &str) -> Result<(), StoreError> {
            Ok(())
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "picsou_secrets_{name}_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&dir).expect("create temp secrets dir");
        dir
    }

    fn env_with(dir: &Path, vars: &[(&str, &str)]) -> Env {
        let mut env = Env::from_map(vec![(
            "PICSOU_SECRETS_DIR".to_owned(),
            dir.to_string_lossy().into_owned(),
        )]);
        for (k, v) in vars {
            env.set((*k).to_owned(), (*v).to_owned());
        }
        env
    }

    fn is_base64_of(secret: &str, bytes: usize) -> bool {
        STANDARD
            .decode(secret)
            .is_ok_and(|decoded| decoded.len() == bytes)
    }

    #[test]
    fn off_mode_keeps_the_secrets_files() {
        let dir = temp_dir("off");
        let store = MemStore::with(&[]);

        let first =
            bootstrap_with(&env_with(&dir, &[]), KeystoreMode::Off, &store).expect("bootstrap");
        let jwt = first.jwt_secret.expect("jwt");
        let crypto = first.crypto_key.expect("crypto");
        assert!(dir.join("jwt_secret").exists());
        assert!(dir.join("crypto_key").exists());
        assert!(store.get("jwt_secret").is_none());

        let second = bootstrap_with(&env_with(&dir, &[]), KeystoreMode::Off, &store)
            .expect("bootstrap again");
        assert_eq!(second.jwt_secret.as_deref(), Some(jwt.as_str()));
        assert_eq!(second.crypto_key.as_deref(), Some(crypto.as_str()));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn auto_mode_migrates_files_into_the_store() {
        let dir = temp_dir("migrate");
        fs::write(
            dir.join("jwt_secret"),
            "file-jwt-value-which-is-long-enough",
        )
        .expect("seed jwt file");
        fs::write(
            dir.join("crypto_key"),
            "file-crypto-value-which-is-long-enough",
        )
        .expect("seed crypto file");
        let store = MemStore::with(&[]);

        let resolved = bootstrap_with(
            &env_with(&dir, &[("PICSOU_OS_KEYSTORE", "auto")]),
            KeystoreMode::Auto,
            &store,
        )
        .expect("bootstrap");
        assert_eq!(
            resolved.jwt_secret.as_deref(),
            Some("file-jwt-value-which-is-long-enough")
        );
        assert!(!dir.join("jwt_secret").exists());
        assert!(!dir.join("crypto_key").exists());
        assert_eq!(
            store.get("jwt_secret").as_deref(),
            Some("file-jwt-value-which-is-long-enough")
        );

        let again = bootstrap_with(
            &env_with(&dir, &[("PICSOU_OS_KEYSTORE", "auto")]),
            KeystoreMode::Auto,
            &store,
        )
        .expect("bootstrap again");
        assert_eq!(again.jwt_secret, resolved.jwt_secret);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn auto_mode_generates_into_the_store_when_nothing_exists() {
        let dir = temp_dir("generate");
        let store = MemStore::with(&[]);

        let resolved = bootstrap_with(
            &env_with(&dir, &[("PICSOU_OS_KEYSTORE", "auto")]),
            KeystoreMode::Auto,
            &store,
        )
        .expect("bootstrap");
        let jwt = resolved.jwt_secret.expect("jwt");
        let crypto = resolved.crypto_key.expect("crypto");
        assert!(is_base64_of(&jwt, 48));
        assert!(is_base64_of(&crypto, 32));
        assert!(!dir.join("jwt_secret").exists());
        assert!(!dir.join("crypto_key").exists());
        assert_eq!(store.get("jwt_secret").as_deref(), Some(jwt.as_str()));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn auto_mode_falls_back_to_files_when_the_store_fails() {
        let dir = temp_dir("fallback");

        let first = bootstrap_with(
            &env_with(&dir, &[("PICSOU_OS_KEYSTORE", "auto")]),
            KeystoreMode::Auto,
            &FailStore,
        )
        .expect("bootstrap falls back");
        assert!(dir.join("jwt_secret").exists());
        let second = bootstrap_with(
            &env_with(&dir, &[("PICSOU_OS_KEYSTORE", "auto")]),
            KeystoreMode::Auto,
            &FailStore,
        )
        .expect("bootstrap falls back again");
        assert_eq!(second.jwt_secret, first.jwt_secret);
        assert_eq!(second.crypto_key, first.crypto_key);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn require_mode_fails_closed_when_the_store_fails() {
        let dir = temp_dir("require-fail");

        let err = bootstrap_with(
            &env_with(&dir, &[("PICSOU_OS_KEYSTORE", "require")]),
            KeystoreMode::Require,
            &FailStore,
        )
        .expect_err("require must fail");
        assert!(err.to_string().contains("JWT_SECRET"), "{err}");
        assert!(!dir.join("jwt_secret").exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn require_mode_migrates_then_reads_from_the_store() {
        let dir = temp_dir("require-migrate");
        fs::write(
            dir.join("jwt_secret"),
            "file-jwt-value-which-is-long-enough",
        )
        .expect("seed jwt file");
        let store = MemStore::with(&[]);

        let resolved = bootstrap_with(
            &env_with(&dir, &[("PICSOU_OS_KEYSTORE", "require")]),
            KeystoreMode::Require,
            &store,
        )
        .expect("bootstrap");
        assert_eq!(
            resolved.jwt_secret.as_deref(),
            Some("file-jwt-value-which-is-long-enough")
        );
        assert!(!dir.join("jwt_secret").exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn require_mode_never_writes_plaintext() {
        let dir = temp_dir("require-env");
        let store = MemStore::with(&[]);

        let resolved = bootstrap_with(
            &env_with(
                &dir,
                &[
                    ("PICSOU_OS_KEYSTORE", "require"),
                    ("JWT_SECRET", "env-jwt-value-which-is-long-enough!!"),
                    (
                        "CRYPTO_ENCRYPTION_KEY",
                        "env-crypto-value-which-is-long-enough",
                    ),
                ],
            ),
            KeystoreMode::Require,
            &store,
        )
        .expect("bootstrap");
        assert_eq!(
            resolved.jwt_secret.as_deref(),
            Some("env-jwt-value-which-is-long-enough!!")
        );
        assert!(dir.read_dir().expect("read dir").next().is_none());
        assert!(store.get("jwt_secret").is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_store_that_loses_writes_is_an_error_in_both_modes() {
        for mode in [KeystoreMode::Auto, KeystoreMode::Require] {
            let dir = temp_dir("amnesiac");
            let err = bootstrap_with(&env_with(&dir, &[]), mode, &AmnesiacStore)
                .expect_err("lost write must fail");
            assert!(err.to_string().contains("two competing values"), "{err}");
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn an_unknown_keystore_mode_is_rejected() {
        let dir = temp_dir("bad-mode");
        let err = bootstrap(&env_with(&dir, &[("PICSOU_OS_KEYSTORE", "tpm")]))
            .expect_err("unknown mode must fail");
        assert!(err.to_string().contains("PICSOU_OS_KEYSTORE"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }
}
