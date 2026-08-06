use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use chrono::NaiveDate;
use rand::RngCore;
use rusqlite::{Connection, OpenFlags, OptionalExtension, backup::Backup};
use thiserror::Error;

use crate::DatabaseKey;

const BACKUP_DIRECTORY: &str = "maestro-backups-v1";
const BACKUP_PREFIX: &str = "maestro-backup-v1-";
const BACKUP_SUFFIX: &str = ".sqlcipher";
const DAILY_SNAPSHOTS: usize = 7;
const STALE_TEMP_AGE: Duration = Duration::from_hours(24);
const TEMP_ATTEMPTS: usize = 128;

#[derive(Debug, Clone)]
pub struct BackupStore {
    source: PathBuf,
    directory: PathBuf,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct BackupResult {
    pub snapshot: PathBuf,
    pub created: bool,
    pub rotation: BackupRotation,
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct BackupRotation {
    pub removed: Vec<PathBuf>,
    pub skipped_untrusted: Vec<PathBuf>,
}

impl BackupStore {
    /// Creates a backup store derived from a Maestro-owned database path.
    ///
    /// Backups are always confined to the fixed `maestro-backups-v1`
    /// directory adjacent to the database. The store never accepts or scans a
    /// vendor configuration/session directory independently.
    ///
    /// # Errors
    ///
    /// Returns [`BackupError`] when the source is missing, non-regular, a
    /// symlink, or its parent cannot be resolved securely.
    pub fn for_database(source: &Path) -> Result<Self, BackupError> {
        let metadata = fs::symlink_metadata(source)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(BackupError::UnsafePath(source.to_path_buf()));
        }
        let source = fs::canonicalize(source)?;
        let parent = source
            .parent()
            .ok_or_else(|| BackupError::UnsafePath(source.clone()))?
            .to_path_buf();
        Ok(Self {
            source,
            directory: parent.join(BACKUP_DIRECTORY),
        })
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Creates or reuses the encrypted snapshot for one UTC calendar day.
    ///
    /// A completed temporary `SQLCipher` database is verified before atomic,
    /// no-clobber publication. At most seven verified Maestro snapshots are
    /// retained.
    ///
    /// # Errors
    ///
    /// Returns [`BackupError`] if backup, verification, publication, or
    /// rotation fails.
    pub fn create_daily_snapshot(
        &self,
        key: &DatabaseKey,
        date: NaiveDate,
    ) -> Result<BackupResult, BackupError> {
        prepare_private_directory(&self.directory)?;
        cleanup_stale_temporaries(&self.directory)?;
        let snapshot = self.directory.join(snapshot_filename(date));

        let existed = match fs::symlink_metadata(&snapshot) {
            Ok(_) => {
                verify_snapshot(&snapshot, key)?;
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };

        let (temporary, file) = create_private_temporary(&self.directory)?;
        let cleanup = TemporaryCleanup::new(temporary);
        drop(file);

        create_online_backup(&self.source, &cleanup.path, key)?;
        verify_snapshot(&cleanup.path, key)?;
        OpenOptions::new()
            .read(true)
            .open(&cleanup.path)?
            .sync_all()?;

        fs::rename(&cleanup.path, &snapshot)?;
        sync_directory(&self.directory)?;
        let rotation = self.rotate(key)?;
        Ok(BackupResult {
            snapshot,
            created: !existed,
            rotation,
        })
    }

    /// Removes snapshots older than the seven newest verified daily backups.
    ///
    /// Only direct, regular files with the exact Maestro filename grammar and
    /// a valid keyed Maestro schema are eligible for deletion.
    ///
    /// # Errors
    ///
    /// Returns [`BackupError`] when the private directory cannot be inspected
    /// or an eligible backup cannot be removed durably.
    pub fn rotate(&self, key: &DatabaseKey) -> Result<BackupRotation, BackupError> {
        prepare_private_directory(&self.directory)?;
        let mut candidates = Vec::new();
        let mut skipped_untrusted = Vec::new();

        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            let path = entry.path();
            let Some(date) = parse_snapshot_filename(&entry.file_name()) else {
                continue;
            };
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || verify_snapshot(&path, key).is_err()
            {
                skipped_untrusted.push(path);
                continue;
            }
            candidates.push((date, path));
        }

        candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.0));
        let mut removed = Vec::new();
        for (_, path) in candidates.into_iter().skip(DAILY_SNAPSHOTS) {
            fs::remove_file(&path)?;
            removed.push(path);
        }
        if !removed.is_empty() {
            sync_directory(&self.directory)?;
        }
        skipped_untrusted.sort();
        Ok(BackupRotation {
            removed,
            skipped_untrusted,
        })
    }
}

/// Verifies encryption, `SQLCipher` page authentication, `SQLite` integrity, and
/// the Maestro schema marker for a published snapshot.
///
/// # Errors
///
/// Returns [`BackupError`] for symlinks, wrong keys, corruption, plaintext
/// `SQLite` databases, or non-Maestro databases.
pub fn verify_snapshot(path: &Path, key: &DatabaseKey) -> Result<(), BackupError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(BackupError::UnsafePath(path.to_path_buf()));
    }
    let connection = open_encrypted(path, key, true)?;
    let cipher_integrity = connection
        .query_row("PRAGMA cipher_integrity_check", [], |row| {
            row.get::<_, String>(0)
        })
        .optional()?;
    if cipher_integrity
        .as_deref()
        .is_some_and(|value| value != "ok")
    {
        return Err(BackupError::IntegrityFailed(
            cipher_integrity.unwrap_or_default(),
        ));
    }
    let integrity = connection.query_row("PRAGMA integrity_check(1)", [], |row| {
        row.get::<_, String>(0)
    })?;
    if integrity != "ok" {
        return Err(BackupError::IntegrityFailed(integrity));
    }
    let maestro_schema = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema \
         WHERE type = 'table' AND name = 'schema_migrations')",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if !maestro_schema {
        return Err(BackupError::NotMaestroDatabase(path.to_path_buf()));
    }
    Ok(())
}

fn create_online_backup(
    source_path: &Path,
    destination_path: &Path,
    key: &DatabaseKey,
) -> Result<(), BackupError> {
    let source = open_encrypted(source_path, key, true)?;
    let mut destination = open_encrypted(destination_path, key, false)?;
    destination.execute_batch(
        "PRAGMA journal_mode = DELETE; \
         PRAGMA synchronous = FULL; \
         PRAGMA cipher_memory_security = ON;",
    )?;
    let backup = Backup::new(&source, &mut destination)?;
    backup.run_to_completion(128, Duration::from_millis(5), None)?;
    drop(backup);
    destination.execute_batch("PRAGMA optimize")?;
    drop(destination);
    Ok(())
}

fn open_encrypted(
    path: &Path,
    key: &DatabaseKey,
    read_only: bool,
) -> Result<Connection, BackupError> {
    let flags = if read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    };
    let connection = Connection::open_with_flags(path, flags)?;
    let encoded = hex::encode(key.expose());
    connection.execute_batch(&format!(
        "PRAGMA key = \"x'{encoded}'\"; PRAGMA cipher_memory_security = ON;"
    ))?;
    let cipher_version = connection
        .query_row("PRAGMA cipher_version", [], |row| row.get::<_, String>(0))
        .optional()?;
    if cipher_version.as_deref().is_none_or(str::is_empty) {
        return Err(BackupError::EncryptionUnavailable);
    }
    connection.query_row("SELECT count(*) FROM sqlite_schema", [], |_| Ok(()))?;
    Ok(connection)
}

fn prepare_private_directory(path: &Path) -> Result<(), BackupError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(BackupError::UnsafePath(path.to_path_buf()));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir(path)?,
        Err(error) => return Err(error.into()),
    }
    restrict_directory(path)?;
    Ok(())
}

fn snapshot_filename(date: NaiveDate) -> String {
    format!("{BACKUP_PREFIX}{}{BACKUP_SUFFIX}", date.format("%Y-%m-%d"))
}

fn parse_snapshot_filename(name: &std::ffi::OsStr) -> Option<NaiveDate> {
    let name = name.to_str()?;
    let date = name
        .strip_prefix(BACKUP_PREFIX)?
        .strip_suffix(BACKUP_SUFFIX)?;
    if date.len() != 10 {
        return None;
    }
    let parsed = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    (parsed.format("%Y-%m-%d").to_string() == date).then_some(parsed)
}

fn create_private_temporary(directory: &Path) -> Result<(PathBuf, fs::File), BackupError> {
    for _ in 0..TEMP_ATTEMPTS {
        let mut random = [0_u8; 16];
        rand::rng().fill_bytes(&mut random);
        let path = directory.join(format!(".maestro-backup-v1-{}.tmp", hex::encode(random)));
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(BackupError::TemporaryFileExhausted)
}

fn cleanup_stale_temporaries(directory: &Path) -> Result<(), BackupError> {
    let now = SystemTime::now();
    let mut removed = false;
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !is_owned_temporary_name(&entry.file_name()) {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        let old_enough = metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= STALE_TEMP_AGE);
        if old_enough {
            fs::remove_file(path)?;
            removed = true;
        }
    }
    if removed {
        sync_directory(directory)?;
    }
    Ok(())
}

fn is_owned_temporary_name(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let Some(random) = name
        .strip_prefix(".maestro-backup-v1-")
        .and_then(|name| name.strip_suffix(".tmp"))
    else {
        return false;
    };
    random.len() == 32 && random.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Debug)]
struct TemporaryCleanup {
    path: PathBuf,
}

impl TemporaryCleanup {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for TemporaryCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
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
fn sync_directory(path: &Path) -> Result<(), std::io::Error> {
    fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

#[derive(Debug, Error)]
pub enum BackupError {
    #[error("SQLCipher support is unavailable; refusing an unencrypted backup")]
    EncryptionUnavailable,
    #[error("backup integrity check failed: {0}")]
    IntegrityFailed(String),
    #[error("backup is not a Maestro database: {}", .0.display())]
    NotMaestroDatabase(PathBuf),
    #[error("could not allocate a private backup temporary file")]
    TemporaryFileExhausted,
    #[error("refusing unsafe backup path: {}", .0.display())]
    UnsafePath(PathBuf),
    #[error("backup filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("backup database operation failed: {0}")]
    Sql(#[from] rusqlite::Error),
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{Duration, SystemTime},
    };

    use chrono::NaiveDate;
    use rusqlite::Connection;
    use tempfile::tempdir;

    use crate::{Database, DatabaseKey};

    use super::{
        BACKUP_PREFIX, BACKUP_SUFFIX, BackupError, BackupStore, snapshot_filename, verify_snapshot,
    };

    #[test]
    fn encrypted_online_backup_is_verified_and_restorable() {
        let directory = tempdir().expect("temporary directory");
        let source_path = directory.path().join("maestro.db");
        let key = DatabaseKey::generate();
        let database = Database::open(&source_path, &key).expect("database opens");
        let marker = "backup-plaintext-leak-marker";
        database
            .connection()
            .execute(
                "INSERT INTO settings(scope, key, value_json, updated_at) \
                 VALUES ('global', 'backup-marker', ?1, ?2)",
                [marker, chrono::Utc::now().to_rfc3339().as_str()],
            )
            .expect("live WAL data inserts");

        let store = BackupStore::for_database(&source_path).expect("backup store");
        let result = store
            .create_daily_snapshot(
                &key,
                NaiveDate::from_ymd_opt(2026, 8, 5).expect("valid date"),
            )
            .expect("snapshot created");
        assert!(result.created);
        verify_snapshot(&result.snapshot, &key).expect("correct key verifies");
        assert!(verify_snapshot(&result.snapshot, &DatabaseKey::generate()).is_err());
        let bytes = fs::read(&result.snapshot).expect("snapshot bytes");
        assert!(
            !bytes
                .windows(marker.len())
                .any(|bytes| bytes == marker.as_bytes())
        );

        let isolated = directory.path().join("isolated-restore.sqlcipher");
        fs::copy(&result.snapshot, &isolated).expect("snapshot copied for restore");
        verify_snapshot(&isolated, &key).expect("isolated copy verifies");
        let restored = open_with_key(&isolated, &key);
        let restored_marker: String = restored
            .query_row(
                "SELECT value_json FROM settings WHERE key = 'backup-marker'",
                [],
                |row| row.get(0),
            )
            .expect("marker restores");
        assert_eq!(restored_marker, marker);

        database
            .connection()
            .execute(
                "UPDATE settings SET value_json = 'refreshed-before-migration' \
                 WHERE key = 'backup-marker'",
                [],
            )
            .expect("source changes later on the same day");
        let refreshed = store
            .create_daily_snapshot(
                &key,
                NaiveDate::from_ymd_opt(2026, 8, 5).expect("valid date"),
            )
            .expect("same-day snapshot refreshes atomically");
        assert!(!refreshed.created);
        let reopened = open_with_key(&refreshed.snapshot, &key);
        let refreshed_marker: String = reopened
            .query_row(
                "SELECT value_json FROM settings WHERE key = 'backup-marker'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(refreshed_marker, "refreshed-before-migration");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(store.directory())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(&result.snapshot).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn rotation_keeps_seven_verified_daily_snapshots_and_ignores_other_files() {
        let directory = tempdir().expect("temporary directory");
        let source_path = directory.path().join("maestro.db");
        let key = DatabaseKey::generate();
        drop(Database::open(&source_path, &key).expect("database opens"));
        let store = BackupStore::for_database(&source_path).expect("backup store");

        for day in 1..=9 {
            store
                .create_daily_snapshot(
                    &key,
                    NaiveDate::from_ymd_opt(2026, 7, day).expect("valid date"),
                )
                .expect("daily snapshot");
        }

        let unrelated = store.directory().join("claude-session.json");
        fs::write(&unrelated, b"vendor-owned fixture").expect("unrelated file");
        let lookalike = store.directory().join(snapshot_filename(
            NaiveDate::from_ymd_opt(2000, 1, 1).unwrap(),
        ));
        fs::write(&lookalike, b"not a Maestro SQLCipher database").expect("lookalike");
        let vendor_sibling = directory.path().join("vendor-session.db");
        fs::write(&vendor_sibling, b"vendor data").expect("vendor sibling");

        let rotation = store.rotate(&key).expect("rotation succeeds");
        assert!(rotation.skipped_untrusted.contains(&lookalike));
        assert!(lookalike.exists());
        assert_eq!(fs::read(&unrelated).unwrap(), b"vendor-owned fixture");
        assert_eq!(fs::read(&vendor_sibling).unwrap(), b"vendor data");

        let verified_count = fs::read_dir(store.directory())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with(BACKUP_PREFIX)
                    && name.ends_with(BACKUP_SUFFIX)
                    && verify_snapshot(&entry.path(), &key).is_ok()
            })
            .count();
        assert_eq!(verified_count, 7);
    }

    #[test]
    fn stale_owned_crash_temp_is_cleaned_without_touching_unrelated_temp() {
        let directory = tempdir().expect("temporary directory");
        let source_path = directory.path().join("maestro.db");
        let key = DatabaseKey::generate();
        drop(Database::open(&source_path, &key).expect("database opens"));
        let store = BackupStore::for_database(&source_path).expect("backup store");
        fs::create_dir(store.directory()).expect("backup directory");
        let stale = store
            .directory()
            .join(".maestro-backup-v1-00000000000000000000000000000000.tmp");
        let unrelated = store.directory().join("vendor-backup.tmp");
        fs::write(&stale, b"interrupted encrypted backup").expect("stale temp");
        fs::write(&unrelated, b"unrelated temp").expect("unrelated temp");
        let old = SystemTime::now() - Duration::from_hours(48);
        let times = fs::FileTimes::new().set_modified(old);
        fs::File::open(&stale)
            .unwrap()
            .set_times(times)
            .expect("stale modification time");
        fs::File::open(&unrelated)
            .unwrap()
            .set_times(times)
            .expect("unrelated modification time");

        store
            .create_daily_snapshot(&key, NaiveDate::from_ymd_opt(2026, 8, 5).unwrap())
            .expect("snapshot creates");

        assert!(!stale.exists());
        assert_eq!(fs::read(unrelated).unwrap(), b"unrelated temp");
    }

    #[cfg(unix)]
    #[test]
    fn hostile_snapshot_symlink_is_never_replaced_or_followed() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().expect("temporary directory");
        let source_path = directory.path().join("maestro.db");
        let key = DatabaseKey::generate();
        drop(Database::open(&source_path, &key).expect("database opens"));
        let store = BackupStore::for_database(&source_path).expect("backup store");
        fs::create_dir(store.directory()).expect("backup directory");
        let victim = directory.path().join("vendor-session");
        fs::write(&victim, b"do not change").expect("victim");
        let snapshot = store.directory().join(snapshot_filename(
            NaiveDate::from_ymd_opt(2026, 8, 5).unwrap(),
        ));
        symlink(&victim, &snapshot).expect("hostile snapshot link");

        let result =
            store.create_daily_snapshot(&key, NaiveDate::from_ymd_opt(2026, 8, 5).unwrap());
        assert!(matches!(result, Err(BackupError::UnsafePath(_))));
        assert_eq!(fs::read(victim).unwrap(), b"do not change");
        assert!(
            fs::symlink_metadata(snapshot)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[test]
    fn source_symlink_is_rejected() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().expect("temporary directory");
        let source_path = directory.path().join("maestro.db");
        let linked = directory.path().join("linked.db");
        let key = DatabaseKey::generate();
        drop(Database::open(&source_path, &key).expect("database opens"));
        symlink(source_path, &linked).expect("source link");
        assert!(matches!(
            BackupStore::for_database(&linked),
            Err(BackupError::UnsafePath(_))
        ));
    }

    fn open_with_key(path: &std::path::Path, key: &DatabaseKey) -> Connection {
        let connection = Connection::open(path).expect("restore opens");
        connection
            .execute_batch(&format!(
                "PRAGMA key = \"x'{}'\";",
                hex::encode(key.expose())
            ))
            .expect("restore key applies");
        connection
    }
}
