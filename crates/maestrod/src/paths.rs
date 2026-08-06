use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use rand::RngCore;

use crate::DaemonError;

const TOKEN_BYTES: usize = 32;
const APPLICATION_DIRECTORY: &str = "com.maestroai.app";

#[derive(Clone, PartialEq, Eq)]
pub struct SecretToken(String);

impl std::fmt::Debug for SecretToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SecretToken([REDACTED])")
    }
}

impl SecretToken {
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn matches(&self, candidate: &str) -> bool {
        if candidate.len() != self.0.len() {
            return false;
        }
        self.0
            .as_bytes()
            .iter()
            .zip(candidate.as_bytes())
            .fold(0_u8, |difference, (expected, actual)| {
                difference | (expected ^ actual)
            })
            == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonPaths {
    pub data_directory: PathBuf,
    pub runtime_directory: PathBuf,
    pub socket: PathBuf,
    pub authentication_token: PathBuf,
    pub database: PathBuf,
    pub database_key_envelope: PathBuf,
    persistence: PersistenceMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PersistenceMode {
    Persistent,
    Ephemeral,
}

impl DaemonPaths {
    /// Resolves Maestro's per-user data and runtime directories.
    ///
    /// # Errors
    ///
    /// Returns an error when the platform does not expose a local data path.
    pub fn discover() -> Result<Self, DaemonError> {
        let data_directory = dirs::data_local_dir()
            .ok_or(DaemonError::PathUnavailable("local data directory"))?
            .join(APPLICATION_DIRECTORY);
        let runtime_directory = dirs::runtime_dir()
            .map_or_else(|| data_directory.join("run"), |path| path.join("maestro"));
        Ok(Self::from_directories(
            data_directory,
            runtime_directory,
            PersistenceMode::Persistent,
        ))
    }

    pub fn isolated(base: impl Into<PathBuf>) -> Self {
        let base = base.into();
        Self::from_directories(
            base.join("data"),
            base.join("run"),
            PersistenceMode::Ephemeral,
        )
    }

    fn from_directories(
        data_directory: PathBuf,
        runtime_directory: PathBuf,
        persistence: PersistenceMode,
    ) -> Self {
        Self {
            socket: runtime_directory.join("maestrod.sock"),
            authentication_token: runtime_directory.join("auth-token-v1"),
            database: data_directory.join("maestro.db"),
            database_key_envelope: data_directory.join("database-key-v1.json"),
            data_directory,
            runtime_directory,
            persistence,
        }
    }

    pub(crate) fn is_ephemeral(&self) -> bool {
        self.persistence == PersistenceMode::Ephemeral
    }

    /// Creates and restricts the application-owned directories.
    ///
    /// # Errors
    ///
    /// Returns an error when a path cannot be created, is a symlink, or cannot
    /// be restricted to the current user.
    pub fn prepare(&self) -> Result<(), DaemonError> {
        create_private_directory(&self.data_directory)?;
        create_private_directory(&self.runtime_directory)?;
        Ok(())
    }

    /// Loads the daemon token, creating a random 256-bit token atomically when
    /// this is the first daemon launch.
    ///
    /// # Errors
    ///
    /// Returns an error for filesystem failures or malformed existing tokens.
    pub fn load_or_create_token(&self) -> Result<SecretToken, DaemonError> {
        self.prepare()?;
        match open_new_private(&self.authentication_token) {
            Ok(mut file) => {
                let mut bytes = [0_u8; TOKEN_BYTES];
                rand::rng().fill_bytes(&mut bytes);
                let encoded = hex::encode(bytes);
                file.write_all(encoded.as_bytes())?;
                file.sync_all()?;
                Ok(SecretToken(encoded))
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let mut encoded = String::new();
                OpenOptions::new()
                    .read(true)
                    .open(&self.authentication_token)?
                    .read_to_string(&mut encoded)?;
                let encoded = encoded.trim().to_owned();
                if encoded.len() != TOKEN_BYTES * 2 || hex::decode(&encoded).is_err() {
                    return Err(DaemonError::InvalidTokenFile);
                }
                restrict_file(&self.authentication_token)?;
                Ok(SecretToken(encoded))
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Loads an existing daemon token without creating one.
    ///
    /// # Errors
    ///
    /// Returns an error when the token is absent, inaccessible, or malformed.
    pub fn load_token(&self) -> Result<SecretToken, DaemonError> {
        let mut encoded = String::new();
        OpenOptions::new()
            .read(true)
            .open(&self.authentication_token)?
            .read_to_string(&mut encoded)?;
        let encoded = encoded.trim().to_owned();
        if encoded.len() != TOKEN_BYTES * 2 || hex::decode(&encoded).is_err() {
            return Err(DaemonError::InvalidTokenFile);
        }
        restrict_file(&self.authentication_token)?;
        Ok(SecretToken(encoded))
    }
}

fn create_private_directory(path: &Path) -> Result<(), std::io::Error> {
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "daemon path is not a real directory",
        ));
    }
    restrict_directory(path)
}

#[cfg(unix)]
fn open_new_private(path: &Path) -> Result<fs::File, std::io::Error> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn open_new_private(path: &Path) -> Result<fs::File, std::io::Error> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

#[cfg(unix)]
fn restrict_directory(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn restrict_directory(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(unix)]
fn restrict_file(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_file(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::DaemonPaths;

    #[test]
    fn token_is_stable_private_and_redacted() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = DaemonPaths::isolated(temporary.path());
        let created = paths.load_or_create_token().expect("token is created");
        let loaded = paths.load_or_create_token().expect("token is loaded");

        assert_eq!(created, loaded);
        assert_eq!(created.expose().len(), 64);
        assert_eq!(format!("{created:?}"), "SecretToken([REDACTED])");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&paths.authentication_token)
                    .expect("metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
}
