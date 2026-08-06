use std::{
    fs,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use thiserror::Error;

use crate::DatabaseKey;

const INITIAL_MIGRATION: &str = include_str!("../migrations/0001_initial.sql");
const PROJECT_UI_STATE_MIGRATION: &str = include_str!("../migrations/0002_project_ui_state.sql");
const RAW_PROTOCOL_CAPTURE_MIGRATION: &str =
    include_str!("../migrations/0003_raw_protocol_capture.sql");
const CURRENT_SCHEMA_VERSION: i64 = 3;

#[derive(Debug)]
pub struct Database {
    connection: Connection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoverySummary {
    pub interrupted_runs: usize,
    pub interrupted_sessions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedProject {
    pub id: String,
    pub display_name: String,
    pub canonical_roots: Vec<String>,
    pub favorite: bool,
    pub last_opened_at: String,
}

impl Database {
    /// Opens or creates an encrypted database and applies pending migrations.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the filesystem cannot be prepared, `SQLCipher`
    /// is unavailable, the key is invalid, or a migration fails.
    pub fn open(path: &Path, key: &DatabaseKey) -> Result<Self, StorageError> {
        if path.exists() {
            probe_schema_compatibility(path, key)?;
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
            restrict_directory(parent)?;
        }

        let connection = Connection::open(path)?;
        apply_key(&connection, key)?;
        verify_sqlcipher(&connection)?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;\
             PRAGMA journal_mode = WAL;\
             PRAGMA synchronous = NORMAL;\
             PRAGMA cipher_memory_security = ON;",
        )?;

        let mut database = Self { connection };
        database.migrate()?;
        restrict_file(path)?;
        Ok(database)
    }

    /// Creates an encrypted in-memory database for tests and transient work.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if `SQLCipher` cannot be initialized or a
    /// migration fails.
    pub fn open_in_memory(key: &DatabaseKey) -> Result<Self, StorageError> {
        let connection = Connection::open_in_memory()?;
        apply_key(&connection, key)?;
        verify_sqlcipher(&connection)?;
        connection
            .execute_batch("PRAGMA foreign_keys = ON; PRAGMA cipher_memory_security = ON;")?;
        let mut database = Self { connection };
        database.migrate()?;
        Ok(database)
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Creates or refreshes a project and its canonical workspace roots.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the transaction cannot be completed or the
    /// number of roots cannot be represented safely.
    pub fn upsert_project(
        &mut self,
        id: &str,
        display_name: &str,
        canonical_roots: &[String],
    ) -> Result<(), StorageError> {
        let transaction = self.connection.transaction()?;
        upsert_project_in_transaction(&transaction, id, display_name, canonical_roots)?;
        transaction.commit()?;
        Ok(())
    }

    /// Creates or refreshes a registration, reusing the project that already
    /// owns the same ordered canonical-root set when one exists.
    ///
    /// The lookup and upsert share one transaction so a timed-out request and
    /// its retry cannot publish two recent-project identities.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the transaction cannot be completed or
    /// the number of roots cannot be represented safely.
    pub fn upsert_project_registration(
        &mut self,
        proposed_id: &str,
        display_name: &str,
        canonical_roots: &[String],
    ) -> Result<String, StorageError> {
        let root_count =
            i64::try_from(canonical_roots.len()).map_err(|_| StorageError::InvalidLimit)?;
        let transaction = self.connection.transaction()?;
        let candidates = if let Some(first_root) = canonical_roots.first() {
            let mut statement = transaction.prepare(
                "SELECT projects.id
                 FROM projects
                 JOIN workspace_roots
                   ON workspace_roots.project_id = projects.id
                  AND workspace_roots.display_order = 0
                 WHERE workspace_roots.canonical_path = ?3
                   AND (SELECT COUNT(*) FROM workspace_roots AS counted
                        WHERE counted.project_id = projects.id) = ?2
                 ORDER BY (projects.id = ?1) DESC,
                          projects.last_opened_at DESC,
                          projects.id",
            )?;
            statement
                .query_map(
                    rusqlite::params![proposed_id, root_count, first_root],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };
        let mut resolved_id = None;
        for candidate in candidates {
            let candidate_roots = {
                let mut statement = transaction.prepare(
                    "SELECT canonical_path FROM workspace_roots
                     WHERE project_id = ?1 ORDER BY display_order",
                )?;
                statement
                    .query_map([&candidate], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?
            };
            if candidate_roots == canonical_roots {
                resolved_id = Some(candidate);
                break;
            }
        }
        let resolved_id = resolved_id.unwrap_or_else(|| proposed_id.to_owned());
        upsert_project_in_transaction(&transaction, &resolved_id, display_name, canonical_roots)?;
        transaction.commit()?;
        Ok(resolved_id)
    }

    /// Updates whether a persisted project is shown as a favorite.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::ProjectNotFound`] when `project_id` has not
    /// been persisted, or another [`StorageError`] when the update fails.
    pub fn set_project_favorite(
        &self,
        project_id: &str,
        favorite: bool,
    ) -> Result<(), StorageError> {
        let changed = self.connection.execute(
            "UPDATE projects
             SET favorite = ?2, updated_at = ?3
             WHERE id = ?1",
            rusqlite::params![project_id, favorite, chrono::Utc::now().to_rfc3339()],
        )?;
        if changed == 0 {
            return Err(StorageError::ProjectNotFound);
        }
        Ok(())
    }

    /// Returns recently opened projects with roots in display order.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::InvalidLimit`] for limits outside `1..=100`, or
    /// another [`StorageError`] when persisted project data cannot be queried.
    pub fn recent_projects(&self, limit: usize) -> Result<Vec<PersistedProject>, StorageError> {
        if limit == 0 || limit > 100 {
            return Err(StorageError::InvalidLimit);
        }
        let mut statement = self.connection.prepare(
            "SELECT id, display_name, favorite, last_opened_at
             FROM projects
             WHERE last_opened_at IS NOT NULL
             ORDER BY favorite DESC, last_opened_at DESC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([i64::try_from(limit).unwrap_or(100)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut projects = Vec::new();
        for row in rows {
            let (id, display_name, favorite, last_opened_at) = row?;
            let mut roots = self.connection.prepare(
                "SELECT canonical_path FROM workspace_roots
                 WHERE project_id = ?1 ORDER BY display_order",
            )?;
            let canonical_roots = roots
                .query_map([&id], |root| root.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            projects.push(PersistedProject {
                id,
                display_name,
                canonical_roots,
                favorite,
                last_opened_at,
            });
        }
        Ok(projects)
    }

    /// Persists opaque, versioned frontend layout JSON per project/window.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when `layout_json` is invalid JSON or the layout
    /// cannot be persisted.
    pub fn save_window_layout(
        &self,
        project_id: &str,
        window_key: &str,
        layout_json: &str,
    ) -> Result<(), StorageError> {
        let _: serde_json::Value = serde_json::from_str(layout_json)?;
        self.connection.execute(
            "INSERT INTO window_layouts (project_id, window_key, layout_json, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(project_id, window_key) DO UPDATE SET
                 layout_json = excluded.layout_json,
                 updated_at = excluded.updated_at",
            rusqlite::params![
                project_id,
                window_key,
                layout_json,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// Loads persisted layout JSON for one project/window.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the layout cannot be queried.
    pub fn window_layout(
        &self,
        project_id: &str,
        window_key: &str,
    ) -> Result<Option<String>, StorageError> {
        self.connection
            .query_row(
                "SELECT layout_json FROM window_layouts
                 WHERE project_id = ?1 AND window_key = ?2",
                rusqlite::params![project_id, window_key],
                |row| row.get(0),
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// Loads one application setting from the encrypted database.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the setting cannot be queried.
    pub fn setting(
        &self,
        scope: &str,
        scope_reference: &str,
        key: &str,
    ) -> Result<Option<String>, StorageError> {
        self.connection
            .query_row(
                "SELECT value_json FROM settings
                 WHERE scope = ?1 AND scope_reference = ?2 AND key = ?3",
                rusqlite::params![scope, scope_reference, key],
                |row| row.get(0),
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// Atomically creates or replaces one application setting in `SQLCipher`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the encrypted database cannot commit the
    /// setting.
    pub fn save_setting(
        &self,
        scope: &str,
        scope_reference: &str,
        key: &str,
        value_json: &str,
    ) -> Result<(), StorageError> {
        self.connection.execute(
            "INSERT INTO settings (scope, scope_reference, key, value_json, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(scope, scope_reference, key) DO UPDATE SET
                 value_json = excluded.value_json,
                 updated_at = excluded.updated_at",
            rusqlite::params![
                scope,
                scope_reference,
                key,
                value_json,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// Registers a daemon-owned terminal before any scrollback segment is
    /// persisted. The project foreign key keeps terminal history scoped to the
    /// same durable project capability as the process launch.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the project is unknown or the encrypted
    /// database cannot commit the terminal metadata.
    pub fn register_terminal_tab(
        &self,
        terminal_id: &str,
        project_id: &str,
        kind: &str,
        title: &str,
        state: &str,
    ) -> Result<(), StorageError> {
        self.connection.execute(
            "INSERT INTO terminal_tabs (
                 id, project_id, session_id, kind, title, state, created_at
             ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
                 title = excluded.title,
                 state = excluded.state",
            rusqlite::params![
                terminal_id,
                project_id,
                kind,
                title,
                state,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// Records one already-encrypted terminal segment. The referenced file is
    /// written atomically before this metadata is committed.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when numeric bounds cannot be represented or
    /// the encrypted database cannot commit the segment metadata.
    pub fn append_terminal_segment(
        &self,
        segment_id: &str,
        terminal_id: &str,
        sequence_start: u64,
        sequence_end: u64,
        byte_count: usize,
        storage_path: &Path,
    ) -> Result<(), StorageError> {
        let sequence_start =
            i64::try_from(sequence_start).map_err(|_| StorageError::InvalidLimit)?;
        let sequence_end = i64::try_from(sequence_end).map_err(|_| StorageError::InvalidLimit)?;
        let byte_count = i64::try_from(byte_count).map_err(|_| StorageError::InvalidLimit)?;
        self.connection.execute(
            "INSERT INTO terminal_segments (
                 id, terminal_tab_id, sequence_start, sequence_end,
                 byte_count, storage_path, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                segment_id,
                terminal_id,
                sequence_start,
                sequence_end,
                byte_count,
                storage_path.to_string_lossy(),
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// Removes the oldest terminal-segment metadata until the owner fits its
    /// byte budget and returns the encrypted files that may now be deleted.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] for invalid stored sizes or when the encrypted
    /// database cannot read or commit the retention transaction.
    pub fn prune_terminal_segments(
        &mut self,
        terminal_id: &str,
        maximum_bytes: u64,
    ) -> Result<Vec<PathBuf>, StorageError> {
        let mut statement = self.connection.prepare(
            "SELECT id, byte_count, storage_path
             FROM terminal_segments
             WHERE terminal_tab_id = ?1
             ORDER BY created_at DESC, sequence_end DESC, id DESC",
        )?;
        let records = statement
            .query_map([terminal_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    PathBuf::from(row.get::<_, String>(2)?),
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);

        let mut retained_bytes = 0_u64;
        let mut expired = Vec::new();
        for (id, byte_count, storage_path) in records {
            let byte_count = u64::try_from(byte_count).map_err(|_| StorageError::InvalidLimit)?;
            if retained_bytes.saturating_add(byte_count) <= maximum_bytes {
                retained_bytes = retained_bytes.saturating_add(byte_count);
            } else {
                expired.push((id, storage_path));
            }
        }
        if expired.is_empty() {
            return Ok(Vec::new());
        }

        let transaction = self.connection.transaction()?;
        for (id, _) in &expired {
            transaction.execute("DELETE FROM terminal_segments WHERE id = ?1", [id])?;
        }
        transaction.commit()?;
        Ok(expired.into_iter().map(|(_, path)| path).collect())
    }

    /// Updates the durable lifecycle state of one terminal tab.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the encrypted database update fails.
    pub fn update_terminal_state(
        &self,
        terminal_id: &str,
        state: &str,
    ) -> Result<(), StorageError> {
        self.connection.execute(
            "UPDATE terminal_tabs SET state = ?2 WHERE id = ?1",
            rusqlite::params![terminal_id, state],
        )?;
        Ok(())
    }

    /// Returns the latest successfully applied Maestro schema version.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the migration table cannot be queried.
    pub fn schema_version(&self) -> Result<i64, StorageError> {
        self.connection
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .map_err(StorageError::from)
    }

    /// Marks work that could not have survived a daemon interruption without
    /// claiming that the underlying process resumed.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the recovery transaction cannot commit.
    pub fn recover_interrupted_work(&mut self) -> Result<RecoverySummary, StorageError> {
        let transaction = self.connection.transaction()?;
        let interrupted_at = chrono::Utc::now().to_rfc3339();
        let interrupted_runs = transaction.execute(
            "UPDATE process_runs
             SET state = 'interrupted',
                 exited_at = COALESCE(exited_at, ?1),
                 recovery_json = '{\"reason\":\"daemon_restart\"}'
             WHERE state IN ('created', 'starting', 'ready', 'running',
                             'awaiting_permission', 'awaiting_user_input',
                             'background', 'interrupting')",
            [&interrupted_at],
        )?;
        let interrupted_sessions = transaction.execute(
            "UPDATE sessions
             SET state = 'interrupted', updated_at = ?1
             WHERE state IN ('created', 'starting', 'ready', 'running',
                             'awaiting_permission', 'awaiting_user_input',
                             'background', 'interrupting')",
            [&interrupted_at],
        )?;
        transaction.execute(
            "UPDATE terminal_tabs
             SET state = 'interrupted'
             WHERE state IN ('running', 'starting')",
            [],
        )?;
        transaction.commit()?;
        Ok(RecoverySummary {
            interrupted_runs,
            interrupted_sessions,
        })
    }

    fn migrate(&mut self) -> Result<(), StorageError> {
        self.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (\
                version INTEGER PRIMARY KEY,\
                applied_at TEXT NOT NULL\
             );",
        )?;

        let latest =
            self.connection
                .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
                    row.get::<_, Option<i64>>(0)
                })?;
        reject_future_schema(latest)?;

        let initial_applied = self
            .connection
            .query_row(
                "SELECT version FROM schema_migrations WHERE version = 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;

        if initial_applied.is_none() {
            let transaction = self.connection.transaction()?;
            transaction.execute_batch(INITIAL_MIGRATION)?;
            transaction.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (1, ?1)",
                [chrono::Utc::now().to_rfc3339()],
            )?;
            transaction.commit()?;
        }

        let project_ui_state_applied = self
            .connection
            .query_row(
                "SELECT version FROM schema_migrations WHERE version = 2",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if project_ui_state_applied.is_none() {
            let transaction = self.connection.transaction()?;
            transaction.execute_batch(PROJECT_UI_STATE_MIGRATION)?;
            transaction.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (2, ?1)",
                [chrono::Utc::now().to_rfc3339()],
            )?;
            transaction.commit()?;
        }

        let raw_protocol_capture_applied = self
            .connection
            .query_row(
                "SELECT version FROM schema_migrations WHERE version = 3",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if raw_protocol_capture_applied.is_none() {
            let transaction = self.connection.transaction()?;
            transaction.execute_batch(RAW_PROTOCOL_CAPTURE_MIGRATION)?;
            transaction.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (3, ?1)",
                [chrono::Utc::now().to_rfc3339()],
            )?;
            transaction.commit()?;
        }

        Ok(())
    }
}

fn upsert_project_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    id: &str,
    display_name: &str,
    canonical_roots: &[String],
) -> Result<(), StorageError> {
    let now = chrono::Utc::now().to_rfc3339();
    transaction.execute(
        "INSERT INTO projects (
             id, display_name, created_at, updated_at, favorite, last_opened_at
         ) VALUES (?1, ?2, ?3, ?3, 0, ?3)
         ON CONFLICT(id) DO UPDATE SET
             display_name = excluded.display_name,
             updated_at = excluded.updated_at,
             last_opened_at = excluded.last_opened_at",
        rusqlite::params![id, display_name, now],
    )?;
    transaction.execute("DELETE FROM workspace_roots WHERE project_id = ?1", [id])?;
    for (index, root) in canonical_roots.iter().enumerate() {
        let display_order = i64::try_from(index).map_err(|_| StorageError::InvalidLimit)?;
        transaction.execute(
            "INSERT INTO workspace_roots (
                 id, project_id, canonical_path, display_order
             ) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![format!("{id}:root:{index}"), id, root, display_order],
        )?;
    }
    Ok(())
}

/// Checks an existing database without enabling WAL or running any DDL.
///
/// A newer Maestro binary may have migrated the database beyond what this
/// binary understands. Detect that with a separate read-only connection before
/// the normal connection applies write-affecting pragmas or migrations.
fn probe_schema_compatibility(path: &Path, key: &DatabaseKey) -> Result<(), StorageError> {
    let wal_path = sidecar_path(path, "-wal");
    match fs::metadata(&wal_path) {
        Ok(metadata) if metadata.len() > 0 => {
            return Err(StorageError::UncheckpointedWal(wal_path));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    // `immutable=1` prevents SQLite from creating, truncating, or unlinking WAL
    // and shared-memory sidecars during this compatibility-only inspection.
    let uri = immutable_database_uri(path);
    let connection = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    apply_key(&connection, key)?;
    verify_sqlcipher(&connection)?;

    let migration_table_exists = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema \
         WHERE type = 'table' AND name = 'schema_migrations')",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if !migration_table_exists {
        return Ok(());
    }

    let latest = connection.query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
        row.get::<_, Option<i64>>(0)
    })?;
    reject_future_schema(latest)
}

fn sidecar_path(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    value.into()
}

#[cfg(unix)]
fn immutable_database_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    encode_immutable_uri(path.as_os_str().as_bytes())
}

#[cfg(not(unix))]
fn immutable_database_uri(path: &Path) -> String {
    encode_immutable_uri(path.to_string_lossy().as_bytes())
}

fn encode_immutable_uri(path: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut uri = String::with_capacity(path.len() + 17);
    uri.push_str("file:");
    for byte in path {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'.' | b'_' | b'~') {
            uri.push(char::from(*byte));
        } else {
            uri.push('%');
            uri.push(char::from(HEX[usize::from(*byte >> 4)]));
            uri.push(char::from(HEX[usize::from(*byte & 0x0f)]));
        }
    }
    uri.push_str("?immutable=1");
    uri
}

fn reject_future_schema(latest: Option<i64>) -> Result<(), StorageError> {
    if let Some(found) = latest
        && found > CURRENT_SCHEMA_VERSION
    {
        return Err(StorageError::UnsupportedSchemaVersion {
            found,
            maximum_supported: CURRENT_SCHEMA_VERSION,
        });
    }
    Ok(())
}

fn apply_key(connection: &Connection, key: &DatabaseKey) -> Result<(), StorageError> {
    let encoded = hex::encode(key.expose());
    connection.execute_batch(&format!("PRAGMA key = \"x'{encoded}'\";"))?;
    Ok(())
}

fn verify_sqlcipher(connection: &Connection) -> Result<(), StorageError> {
    let version = connection
        .query_row("PRAGMA cipher_version", [], |row| row.get::<_, String>(0))
        .optional()?;
    match version {
        Some(version) if !version.trim().is_empty() => Ok(()),
        _ => Err(StorageError::EncryptionUnavailable),
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
fn restrict_file(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_file(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("SQLCipher support is unavailable; refusing to open unencrypted storage")]
    EncryptionUnavailable,
    #[error("filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("database schema version {found} is newer than supported version {maximum_supported}")]
    UnsupportedSchemaVersion { found: i64, maximum_supported: i64 },
    #[error(
        "database has an uncheckpointed WAL at {}; refusing a side-effect-free compatibility probe",
        .0.display()
    )]
    UncheckpointedWal(std::path::PathBuf),
    #[error("storage operation limit is invalid")]
    InvalidLimit,
    #[error("the requested project is not persisted")]
    ProjectNotFound,
    #[error("stored JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("database operation failed: {0}")]
    Sql(#[from] rusqlite::Error),
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use tempfile::tempdir;

    use crate::{Database, DatabaseKey, StorageError};

    #[test]
    fn migration_creates_versioned_encrypted_database() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("maestro.db");
        let key = DatabaseKey::generate();
        let database = Database::open(&path, &key).expect("encrypted database opens");
        let marker = "maestro-plaintext-leak-marker";
        let updated_at = chrono::Utc::now().to_rfc3339();

        database
            .connection()
            .execute(
                "INSERT INTO settings (scope, key, value_json, updated_at) VALUES ('global', 'marker', ?1, ?2)",
                rusqlite::params![marker, updated_at],
            )
            .expect("marker inserts");
        assert_eq!(database.schema_version().expect("schema version"), 3);
        drop(database);

        let bytes = fs::read(path).expect("database bytes");
        assert!(
            !bytes
                .windows(marker.len())
                .any(|window| window == marker.as_bytes())
        );
    }

    #[test]
    fn wrong_key_cannot_reopen_database() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("maestro.db");
        let original_key = DatabaseKey::generate();
        drop(Database::open(&path, &original_key).expect("database opens"));

        assert!(Database::open(&path, &DatabaseKey::generate()).is_err());
    }

    #[test]
    fn encrypted_settings_round_trip_and_replace_without_a_schema_change() {
        let key = DatabaseKey::generate();
        let database = Database::open_in_memory(&key).expect("database opens");

        assert_eq!(
            database
                .setting("global", "", "keyboard.shortcuts")
                .expect("missing setting queries"),
            None
        );
        database
            .save_setting(
                "global",
                "",
                "keyboard.shortcuts",
                r#"{"openProject":"Mod+O"}"#,
            )
            .expect("setting saves");
        database
            .save_setting(
                "global",
                "",
                "keyboard.shortcuts",
                r#"{"openProject":"Mod+L"}"#,
            )
            .expect("setting replaces");

        assert_eq!(
            database
                .setting("global", "", "keyboard.shortcuts")
                .expect("setting loads")
                .as_deref(),
            Some(r#"{"openProject":"Mod+L"}"#)
        );
        assert_eq!(database.schema_version().expect("schema version"), 3);
    }

    #[test]
    fn future_schema_versions_are_rejected_without_modification() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("maestro.db");
        let key = DatabaseKey::generate();
        let database = Database::open(&path, &key).expect("database opens");
        database
            .connection()
            .execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (4, ?1)",
                [chrono::Utc::now().to_rfc3339()],
            )
            .expect("future marker inserts");
        database
            .connection()
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .expect("future marker checkpoints");
        drop(database);

        let before = StorageFiles::snapshot(&path);

        let result = Database::open(&path, &key);
        assert!(
            matches!(
                result,
                Err(StorageError::UnsupportedSchemaVersion {
                    found: 4,
                    maximum_supported: 3,
                })
            ),
            "unexpected open result: {result:?}"
        );
        let after = StorageFiles::snapshot(&path);
        before.assert_unchanged(&after);
    }

    #[test]
    fn uncheckpointed_wal_is_rejected_without_touching_sidecars() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("maestro.db");
        let key = DatabaseKey::generate();
        let database = Database::open(&path, &key).expect("database opens");
        database
            .connection()
            .execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (4, ?1)",
                [chrono::Utc::now().to_rfc3339()],
            )
            .expect("future marker enters WAL");
        let before = StorageFiles::snapshot(&path);
        assert!(before.wal.as_ref().is_some_and(|wal| !wal.is_empty()));

        let result = Database::open(&path, &key);
        assert!(matches!(result, Err(StorageError::UncheckpointedWal(_))));
        let after = StorageFiles::snapshot(&path);
        before.assert_unchanged(&after);
    }

    #[test]
    fn immutable_probe_handles_uri_metacharacters_in_paths() {
        let directory = tempdir().expect("temporary directory");
        let path = directory
            .path()
            .join("space ?# percent %")
            .join("maestro.db");
        let key = DatabaseKey::generate();
        drop(Database::open(&path, &key).expect("database opens"));
        drop(Database::open(&path, &key).expect("database reopens"));
    }

    #[test]
    fn restart_recovery_marks_only_unfinished_work_interrupted() {
        let key = DatabaseKey::generate();
        let mut database = Database::open_in_memory(&key).expect("database opens");
        let now = chrono::Utc::now().to_rfc3339();
        database
            .connection()
            .execute(
                "INSERT INTO projects (id, display_name, created_at, updated_at)
                 VALUES ('project-1', 'Project', ?1, ?1)",
                [&now],
            )
            .expect("project inserts");
        database
            .connection()
            .execute(
                "INSERT INTO sessions
                 (id, project_id, agent_kind, integration_mode, state, created_at, updated_at)
                 VALUES ('session-running', 'project-1', 'fake', 'structured', 'running', ?1, ?1),
                        ('session-complete', 'project-1', 'fake', 'structured', 'completed', ?1, ?1)",
                [&now],
            )
            .expect("sessions insert");
        database
            .connection()
            .execute(
                "INSERT INTO process_runs
                 (id, session_id, invocation_json, channel, state, started_at)
                 VALUES ('run-running', 'session-running', '{}', 'structured', 'running', ?1),
                        ('run-complete', 'session-complete', '{}', 'structured', 'completed', ?1)",
                [&now],
            )
            .expect("runs insert");

        let summary = database
            .recover_interrupted_work()
            .expect("recovery commits");

        assert_eq!(summary.interrupted_runs, 1);
        assert_eq!(summary.interrupted_sessions, 1);
        let running_session: String = database
            .connection()
            .query_row(
                "SELECT state FROM sessions WHERE id = 'session-running'",
                [],
                |row| row.get(0),
            )
            .expect("session reads");
        let complete_run: String = database
            .connection()
            .query_row(
                "SELECT state FROM process_runs WHERE id = 'run-complete'",
                [],
                |row| row.get(0),
            )
            .expect("run reads");
        assert_eq!(running_session, "interrupted");
        assert_eq!(complete_run, "completed");
    }

    #[test]
    fn encrypted_project_favorite_and_window_state_survive_restart() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("maestro.db");
        let key = DatabaseKey::generate();
        let mut database = Database::open(&path, &key).expect("database opens");
        let roots = vec![
            "/workspace/primary".to_owned(),
            "/workspace/documentation".to_owned(),
        ];
        database
            .upsert_project("project-1", "Maestro", &roots)
            .expect("project persists");
        database
            .set_project_favorite("project-1", true)
            .expect("favorite persists");
        database
            .save_window_layout("project-1", "main", r#"{"version":1,"sidebarOpen":true}"#)
            .expect("layout persists");
        assert!(matches!(
            database.set_project_favorite("missing-project", true),
            Err(StorageError::ProjectNotFound)
        ));
        drop(database);

        let database = Database::open(&path, &key).expect("database reopens");
        let projects = database.recent_projects(20).expect("projects load");
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].id, "project-1");
        assert_eq!(projects[0].canonical_roots, roots);
        assert!(projects[0].favorite);
        assert_eq!(
            database
                .window_layout("project-1", "main")
                .expect("layout loads")
                .as_deref(),
            Some(r#"{"version":1,"sidebarOpen":true}"#)
        );
    }

    #[test]
    fn registration_retry_reuses_exact_multi_root_identity_and_ui_state() {
        let key = DatabaseKey::generate();
        let mut database = Database::open_in_memory(&key).expect("database opens");
        let roots = vec![
            "/workspace/documentation".to_owned(),
            "/workspace/primary".to_owned(),
        ];
        let first_id = database
            .upsert_project_registration("project-first", "First name", &roots)
            .expect("first registration persists");
        assert_eq!(first_id, "project-first");
        database
            .set_project_favorite(&first_id, true)
            .expect("favorite persists");
        database
            .save_window_layout(&first_id, "main", r#"{"version":1}"#)
            .expect("layout persists");

        let retry_id = database
            .upsert_project_registration("project-retry", "Retried name", &roots)
            .expect("retry resolves");

        assert_eq!(retry_id, first_id);
        let projects = database.recent_projects(10).expect("projects load");
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].id, first_id);
        assert_eq!(projects[0].display_name, "Retried name");
        assert_eq!(projects[0].canonical_roots, roots);
        assert!(projects[0].favorite);
        assert_eq!(
            database
                .window_layout(&first_id, "main")
                .expect("layout loads")
                .as_deref(),
            Some(r#"{"version":1}"#)
        );

        let distinct_roots = vec![
            "/workspace/documentation".to_owned(),
            "/workspace/secondary".to_owned(),
        ];
        assert_eq!(
            database
                .upsert_project_registration("project-distinct", "Distinct roots", &distinct_roots,)
                .expect("different root set persists"),
            "project-distinct"
        );
        assert_eq!(
            database.recent_projects(10).expect("projects load").len(),
            2
        );
    }

    #[derive(Debug, Eq, PartialEq)]
    struct StorageFiles {
        database: Option<Vec<u8>>,
        wal: Option<Vec<u8>>,
        shared_memory: Option<Vec<u8>>,
        rollback_journal: Option<Vec<u8>>,
    }

    impl StorageFiles {
        fn snapshot(path: &Path) -> Self {
            Self {
                database: read_if_present(path),
                wal: read_if_present(&sidecar(path, "-wal")),
                shared_memory: read_if_present(&sidecar(path, "-shm")),
                rollback_journal: read_if_present(&sidecar(path, "-journal")),
            }
        }

        fn assert_unchanged(&self, after: &Self) {
            assert!(
                self.database == after.database,
                "read-only probe modified the database bytes"
            );
            assert!(
                self.wal == after.wal,
                "read-only probe modified or created the WAL sidecar"
            );
            assert!(
                self.shared_memory == after.shared_memory,
                "read-only probe modified or created the shared-memory sidecar"
            );
            assert!(
                self.rollback_journal == after.rollback_journal,
                "read-only probe modified or created the rollback journal"
            );
        }
    }

    fn sidecar(path: &Path, suffix: &str) -> std::path::PathBuf {
        let mut value = path.as_os_str().to_os_string();
        value.push(suffix);
        value.into()
    }

    fn read_if_present(path: &Path) -> Option<Vec<u8>> {
        match fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("failed to snapshot {}: {error}", path.display()),
        }
    }
}
