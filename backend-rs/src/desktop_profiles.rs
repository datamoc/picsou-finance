//! Isolated standalone profiles registry (`profiles.json`) for Picsou Desktop.
//!
//! Stores a registry at `<data_root>/profiles.json`:
//! - Automatically adopts an existing `<data_root>/picsou.db` as the `"default"` profile so
//!   existing installations upgrade with zero data migration.
//! - Additional profiles live in isolated subdirectories `<data_root>/profiles/<id>/` with
//!   their own `picsou.db`, `secrets/`, and `backups/`.

use std::fs;
use std::path::{Path, PathBuf};

use axum::extract::{Path as AxumPath, State};
use chrono::{DateTime, Utc};
use http::StatusCode;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;
use crate::auth::{Auth, Role};
use crate::error::{AppError, AppResult};
use crate::http::json::{Errors, Json, Validate};
use crate::routes::Routes;

pub const REGISTRY_FILENAME: &str = "profiles.json";
pub const DEFAULT_PROFILE_ID: &str = "default";

pub fn register(routes: &mut Routes) {
    routes
        .get("/api/profiles", list_profiles)
        .post("/api/profiles", create_profile)
        .patch("/api/profiles/{id}", rename_profile)
        .delete("/api/profiles/{id}", delete_profile)
        .post("/api/profiles/{id}/activate", activate_profile);
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProfileEntry {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileRegistry {
    pub active_profile_id: String,
    pub profiles: Vec<ProfileEntry>,
}

impl ProfileRegistry {
    pub fn load_or_init(root: &Path, now: DateTime<Utc>) -> std::io::Result<Self> {
        let path = root.join(REGISTRY_FILENAME);
        if path.exists() {
            let raw = fs::read_to_string(&path)?;
            if let Ok(reg) = serde_json::from_str::<Self>(&raw)
                && !reg.profiles.is_empty()
            {
                return Ok(reg);
            }
        }
        let reg = Self {
            active_profile_id: DEFAULT_PROFILE_ID.to_owned(),
            profiles: vec![ProfileEntry {
                id: DEFAULT_PROFILE_ID.to_owned(),
                name: "Principal".to_owned(),
                is_default: true,
                created_at: now,
            }],
        };
        reg.save(root)?;
        Ok(reg)
    }

    pub fn save(&self, root: &Path) -> std::io::Result<()> {
        fs::create_dir_all(root)?;
        let path = root.join(REGISTRY_FILENAME);
        let tmp = root.join("profiles.json.tmp");
        let raw = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        fs::write(&tmp, raw)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Resolves the active profile's data directory. The `"default"` profile maps directly to
    /// `root` (preserving existing `picsou.db`, `secrets/`, `backups/` layout); any additional
    /// profile lives under `root/profiles/<id>`.
    pub fn active_data_dir(&self, root: &Path) -> PathBuf {
        Self::profile_data_dir(root, &self.active_profile_id)
    }

    pub fn profile_data_dir(root: &Path, profile_id: &str) -> PathBuf {
        if profile_id == DEFAULT_PROFILE_ID {
            root.to_path_buf()
        } else {
            root.join("profiles").join(profile_id)
        }
    }
}

/// Resolves the active data directory for the Tauri desktop supervisor before starting the backend.
/// Also applies any staged `.restore-pending` SQLite database swap safely before opening SQLite.
pub fn resolve_desktop_active_dir(root: &Path) -> PathBuf {
    let active = ProfileRegistry::load_or_init(root, Utc::now())
        .map(|reg| reg.active_data_dir(root))
        .unwrap_or_else(|_| root.to_path_buf());
    apply_pending_restore_if_present(&active.join("picsou.db"));
    active
}

/// If an encrypted backup restore staged `<db>.restore-pending`, back up the current `<db>` and
/// swap the staged database in atomically before SQLite opens.
pub fn apply_pending_restore_if_present(db_path: &Path) {
    let mut pending_os = db_path.as_os_str().to_os_string();
    pending_os.push(".restore-pending");
    let pending = PathBuf::from(pending_os);
    if !pending.exists() {
        return;
    }
    if db_path.exists() {
        let mut prev_os = db_path.as_os_str().to_os_string();
        prev_os.push(".pre-restore");
        let _ = fs::copy(db_path, PathBuf::from(prev_os));
    }
    // Remove stale WAL/SHM files before replacing the main DB file.
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = db_path.as_os_str().to_os_string();
        sidecar.push(suffix);
        let _ = fs::remove_file(PathBuf::from(sidecar));
    }
    if let Err(err) = fs::rename(&pending, db_path) {
        tracing::error!("Failed to swap staged restore backup into {}: {err}", db_path.display());
    } else {
        tracing::info!("Applied staged encrypted backup restore to {}", db_path.display());
    }
}

fn data_root(state: &AppState) -> Result<PathBuf, AppError> {
    if let Ok(root) = std::env::var("PICSOU_PROFILES_ROOT")
        && !root.trim().is_empty()
    {
        return Ok(PathBuf::from(root));
    }
    if let Some(sq) = state.db.as_sqlite()
        && let Some(db_path) = sq.file_path()
        && let Some(parent) = db_path.parent()
    {
        // If DB is inside `<root>/profiles/<id>/picsou.db`, walk back up two levels.
        if parent
            .parent()
            .and_then(|p| p.file_name())
            .is_some_and(|n| n == "profiles")
            && let Some(root) = parent.parent().and_then(|p| p.parent())
        {
            return Ok(root.to_path_buf());
        }
        return Ok(parent.to_path_buf());
    }
    Err(AppError::BadRequest(
        "Profiles are only available in standalone SQLite mode.".to_owned(),
    ))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProfilesResponse {
    active_profile_id: String,
    profiles: Vec<ProfileEntry>,
}

async fn list_profiles(
    State(state): State<AppState>,
    _auth: Auth,
) -> AppResult<axum::Json<ProfilesResponse>> {
    let root = data_root(&state)?;
    let reg = ProfileRegistry::load_or_init(&root, state.clock.now())
        .map_err(|e| AppError::internal(format!("Failed to load profiles registry: {e}")))?;
    Ok(axum::Json(ProfilesResponse {
        active_profile_id: reg.active_profile_id,
        profiles: reg.profiles,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProfileNameRequest {
    name: String,
}

impl Validate for ProfileNameRequest {
    fn validate(&self, errors: &mut Errors) {
        let trimmed = self.name.trim();
        if trimmed.is_empty() || trimmed.chars().count() > 60 {
            errors.add("name", "Profile name must be between 1 and 60 characters");
        }
    }
}

async fn create_profile(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<ProfileNameRequest>,
) -> AppResult<(StatusCode, axum::Json<ProfileEntry>)> {
    if auth.user.role != Role::Admin {
        return Err(AppError::Forbidden("Admin access required".to_owned()));
    }
    let root = data_root(&state)?;
    let now = state.clock.now();
    let mut reg = ProfileRegistry::load_or_init(&root, now)
        .map_err(|e| AppError::internal(format!("Failed to load profiles registry: {e}")))?;
    let entry = ProfileEntry {
        id: Uuid::new_v4().to_string(),
        name: req.name.trim().to_owned(),
        is_default: false,
        created_at: now,
    };
    let dir = ProfileRegistry::profile_data_dir(&root, &entry.id);
    fs::create_dir_all(&dir)
        .map_err(|e| AppError::internal(format!("Failed to create profile directory: {e}")))?;
    reg.profiles.push(entry.clone());
    reg.save(&root)
        .map_err(|e| AppError::internal(format!("Failed to save profiles registry: {e}")))?;
    Ok((StatusCode::CREATED, axum::Json(entry)))
}

async fn rename_profile(
    State(state): State<AppState>,
    auth: Auth,
    AxumPath(id): AxumPath<String>,
    Json(req): Json<ProfileNameRequest>,
) -> AppResult<axum::Json<ProfileEntry>> {
    if auth.user.role != Role::Admin {
        return Err(AppError::Forbidden("Admin access required".to_owned()));
    }
    let root = data_root(&state)?;
    let mut reg = ProfileRegistry::load_or_init(&root, state.clock.now())
        .map_err(|e| AppError::internal(format!("Failed to load profiles registry: {e}")))?;
    let entry = reg
        .profiles
        .iter_mut()
        .find(|p| p.id == id)
        .ok_or_else(|| AppError::NotFound("Profile not found".to_owned()))?;
    entry.name = req.name.trim().to_owned();
    let updated = entry.clone();
    reg.save(&root)
        .map_err(|e| AppError::internal(format!("Failed to save profiles registry: {e}")))?;
    Ok(axum::Json(updated))
}

async fn delete_profile(
    State(state): State<AppState>,
    auth: Auth,
    AxumPath(id): AxumPath<String>,
) -> AppResult<StatusCode> {
    if auth.user.role != Role::Admin {
        return Err(AppError::Forbidden("Admin access required".to_owned()));
    }
    if id == DEFAULT_PROFILE_ID {
        return Err(AppError::BadRequest(
            "The default profile cannot be deleted.".to_owned(),
        ));
    }
    let root = data_root(&state)?;
    let mut reg = ProfileRegistry::load_or_init(&root, state.clock.now())
        .map_err(|e| AppError::internal(format!("Failed to load profiles registry: {e}")))?;
    if reg.active_profile_id == id {
        return Err(AppError::BadRequest(
            "Cannot delete the currently active profile.".to_owned(),
        ));
    }
    let before = reg.profiles.len();
    reg.profiles.retain(|p| p.id != id);
    if reg.profiles.len() == before {
        return Err(AppError::NotFound("Profile not found".to_owned()));
    }
    let dir = ProfileRegistry::profile_data_dir(&root, &id);
    if dir.exists() {
        let _ = fs::remove_dir_all(&dir);
    }
    reg.save(&root)
        .map_err(|e| AppError::internal(format!("Failed to save profiles registry: {e}")))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ActivateProfileResponse {
    active_profile_id: String,
    restart_required: bool,
}

async fn activate_profile(
    State(state): State<AppState>,
    auth: Auth,
    AxumPath(id): AxumPath<String>,
) -> AppResult<axum::Json<ActivateProfileResponse>> {
    if auth.user.role != Role::Admin {
        return Err(AppError::Forbidden("Admin access required".to_owned()));
    }
    let root = data_root(&state)?;
    let mut reg = ProfileRegistry::load_or_init(&root, state.clock.now())
        .map_err(|e| AppError::internal(format!("Failed to load profiles registry: {e}")))?;
    if !reg.profiles.iter().any(|p| p.id == id) {
        return Err(AppError::NotFound("Profile not found".to_owned()));
    }
    let changed = reg.active_profile_id != id;
    reg.active_profile_id = id.clone();
    reg.save(&root)
        .map_err(|e| AppError::internal(format!("Failed to save profiles registry: {e}")))?;
    Ok(axum::Json(ActivateProfileResponse {
        active_profile_id: id,
        restart_required: changed,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_registry_initializes_adopts_default_and_switches_dirs() {
        let tmp = std::env::temp_dir().join(format!("picsou-profiles-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let now = Utc::now();

        let mut reg = ProfileRegistry::load_or_init(&tmp, now).unwrap();
        assert_eq!(reg.active_profile_id, DEFAULT_PROFILE_ID);
        assert_eq!(reg.active_data_dir(&tmp), tmp);

        reg.profiles.push(ProfileEntry {
            id: "pro".to_owned(),
            name: "Pro".to_owned(),
            is_default: false,
            created_at: now,
        });
        reg.active_profile_id = "pro".to_owned();
        reg.save(&tmp).unwrap();

        let reloaded = ProfileRegistry::load_or_init(&tmp, now).unwrap();
        assert_eq!(reloaded.active_profile_id, "pro");
        assert_eq!(
            reloaded.active_data_dir(&tmp),
            tmp.join("profiles").join("pro")
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn pending_restore_swaps_database_file_atomically() {
        let tmp = std::env::temp_dir().join(format!("picsou-restore-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let db = tmp.join("picsou.db");
        fs::write(&db, b"old-db").unwrap();
        fs::write(tmp.join("picsou.db.restore-pending"), b"new-db").unwrap();

        apply_pending_restore_if_present(&db);

        assert_eq!(fs::read(&db).unwrap(), b"new-db");
        assert_eq!(fs::read(tmp.join("picsou.db.pre-restore")).unwrap(), b"old-db");
        assert!(!tmp.join("picsou.db.restore-pending").exists());
        let _ = fs::remove_dir_all(&tmp);
    }
}
