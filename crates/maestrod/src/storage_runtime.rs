use std::{
    fmt, fs,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use chrono::Utc;
use maestro_domain::{
    AgentKind, EventEnvelope, IntegrationMode, ProjectId, RunId, SessionId, SessionState,
    TerminalId,
};
use maestro_protocol::{StorageStatus, StorageUnlockMode};
use maestro_storage::{
    BackupError, BackupStore, Database, DatabaseKey, DatabaseKeyStore, KeyStoreError, OsKeyStore,
    PassphraseKeyStore, PersistedProject, PersistedRawProtocolCapture, PersistedRunExit,
    PersistedSessionMetadata, PersistedSessionSummary, RetentionError, RetentionPolicies,
    SessionStore, SessionStoreError, StorageError, execute_retention_batch, plan_retention,
};
use rand::RngCore;
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::DaemonPaths;

pub(crate) struct StorageRuntime {
    paths: DaemonPaths,
    state: Mutex<StorageState>,
}

enum StorageState {
    Ready(ReadyStorage),
    PassphraseRequired(StorageUnlockMode),
    Unavailable,
}

struct ReadyStorage {
    database: Database,
    key: Option<DatabaseKey>,
}

const TERMINAL_SEGMENTS_DIRECTORY: &str = "terminal-segments-v1";
const RAW_SEGMENTS_DIRECTORY: &str = "raw-segments-v1";
const DEBUG_LOGS_DIRECTORY: &str = "debug-logs-v1";
const TERMINAL_SEGMENT_MAGIC: &[u8; 8] = b"MTRMSEG1";
const TERMINAL_SEGMENT_AAD: &[u8] = b"com.maestroai.app/terminal-segment-v1";
const MAX_TERMINAL_SCROLLBACK_BYTES: u64 = 10 * 1024 * 1024;

impl fmt::Debug for StorageRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StorageRuntime")
            .field("status", &self.snapshot().0)
            .finish_non_exhaustive()
    }
}

impl StorageRuntime {
    pub(crate) fn initialize(paths: DaemonPaths) -> Result<Self, StorageError> {
        let state = if paths.is_ephemeral() {
            let key = DatabaseKey::generate();
            ready_database(Database::open_in_memory(&key)?, None)?
        } else {
            initialize_persistent(&paths)
        };
        Ok(Self {
            paths,
            state: Mutex::new(state),
        })
    }

    #[cfg(test)]
    pub(crate) fn persistent_for_test(
        paths: DaemonPaths,
        key: DatabaseKey,
    ) -> Result<Self, StorageRuntimeError> {
        paths
            .prepare()
            .map_err(|error| StorageRuntimeError::Io(std::io::Error::other(error.to_string())))?;
        let database = Database::open(&paths.database, &key)?;
        Ok(Self {
            paths,
            state: Mutex::new(ready_database(database, Some(key))?),
        })
    }

    #[cfg(test)]
    pub(crate) fn terminal_segment_paths(
        &self,
        terminal_id: TerminalId,
    ) -> Result<Vec<PathBuf>, StorageRuntimeError> {
        self.with_ready_database(|database| {
            let mut statement = database.connection().prepare(
                "SELECT storage_path FROM terminal_segments
                 WHERE terminal_tab_id = ?1 ORDER BY sequence_start",
            )?;
            statement
                .query_map([terminal_id.to_string()], |row| {
                    row.get::<_, String>(0).map(PathBuf::from)
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(StorageError::from)
        })
    }

    pub(crate) fn snapshot(&self) -> (StorageStatus, Option<i64>) {
        let Ok(state) = self.state.lock() else {
            return (StorageStatus::Unavailable, None);
        };
        match &*state {
            StorageState::Ready(ready) => {
                (StorageStatus::Ready, ready.database.schema_version().ok())
            }
            StorageState::PassphraseRequired(mode) => {
                (StorageStatus::PassphraseRequired { mode: *mode }, None)
            }
            StorageState::Unavailable => (StorageStatus::Unavailable, None),
        }
    }

    pub(crate) fn unlock(&self, passphrase: String) -> Result<(), StorageRuntimeError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| StorageRuntimeError::Unavailable)?;
        let mode = match &*state {
            StorageState::Ready(_) => return Ok(()),
            StorageState::PassphraseRequired(mode) => *mode,
            StorageState::Unavailable => return Err(StorageRuntimeError::Unavailable),
        };

        let key_store = PassphraseKeyStore::new(&self.paths.database_key_envelope, passphrase)?;
        let key = match key_store.load_or_create() {
            Ok(key) => key,
            Err(error) => {
                *state = StorageState::PassphraseRequired(mode);
                return Err(error.into());
            }
        };
        let ready = match open_persistent_database(&self.paths, key) {
            Ok(ready) => ready,
            Err(error) => {
                *state = StorageState::PassphraseRequired(StorageUnlockMode::Unlock);
                return Err(error);
            }
        };
        *state = StorageState::Ready(ready);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn upsert_project(
        &self,
        id: &str,
        display_name: &str,
        canonical_roots: &[String],
    ) -> Result<(), StorageRuntimeError> {
        self.with_ready_database(|database| {
            database.upsert_project(id, display_name, canonical_roots)
        })
    }

    pub(crate) fn upsert_project_registration(
        &self,
        proposed_id: &str,
        display_name: &str,
        canonical_roots: &[String],
    ) -> Result<String, StorageRuntimeError> {
        self.with_ready_database(|database| {
            database.upsert_project_registration(proposed_id, display_name, canonical_roots)
        })
    }

    pub(crate) fn recent_projects(
        &self,
        limit: usize,
    ) -> Result<Vec<PersistedProject>, StorageRuntimeError> {
        self.with_ready_database(|database| database.recent_projects(limit))
    }

    pub(crate) fn set_project_favorite(
        &self,
        project_id: &str,
        favorite: bool,
    ) -> Result<(), StorageRuntimeError> {
        self.with_ready_database(|database| database.set_project_favorite(project_id, favorite))
    }

    pub(crate) fn save_window_layout(
        &self,
        project_id: &str,
        window_key: &str,
        layout_json: &str,
    ) -> Result<(), StorageRuntimeError> {
        self.with_ready_database(|database| {
            database.save_window_layout(project_id, window_key, layout_json)
        })
    }

    pub(crate) fn window_layout(
        &self,
        project_id: &str,
        window_key: &str,
    ) -> Result<Option<String>, StorageRuntimeError> {
        self.with_ready_database(|database| database.window_layout(project_id, window_key))
    }

    pub(crate) fn setting(
        &self,
        scope: &str,
        scope_reference: &str,
        key: &str,
    ) -> Result<Option<String>, StorageRuntimeError> {
        self.with_ready_database(|database| database.setting(scope, scope_reference, key))
    }

    pub(crate) fn save_setting(
        &self,
        scope: &str,
        scope_reference: &str,
        key: &str,
        value_json: &str,
    ) -> Result<(), StorageRuntimeError> {
        self.with_ready_database(|database| {
            database.save_setting(scope, scope_reference, key, value_json)
        })
    }

    pub(crate) fn register_terminal(
        &self,
        project_id: ProjectId,
        terminal_id: TerminalId,
        kind: &str,
        title: &str,
    ) -> Result<(), StorageRuntimeError> {
        self.with_ready_database(|database| {
            database.register_terminal_tab(
                &terminal_id.to_string(),
                &project_id.to_string(),
                kind,
                title,
                "running",
            )
        })
    }

    pub(crate) fn persist_terminal_segment(
        &self,
        terminal_id: TerminalId,
        sequence_start: u64,
        sequence_end: u64,
        data: &[u8],
    ) -> Result<(), StorageRuntimeError> {
        if data.is_empty() {
            return Ok(());
        }
        let directory = prepare_private_directory(
            &self.paths.data_directory.join(TERMINAL_SEGMENTS_DIRECTORY),
        )?;
        let segment_id = uuid::Uuid::new_v4().to_string();
        let storage_path = directory.join(format!("{segment_id}.segment"));
        let mut state = self
            .state
            .lock()
            .map_err(|_| StorageRuntimeError::Unavailable)?;
        let StorageState::Ready(ready) = &mut *state else {
            return Err(StorageRuntimeError::Unavailable);
        };
        let Some(key) = ready.key.as_ref() else {
            return Ok(());
        };
        let aad = terminal_segment_aad(terminal_id, sequence_start, sequence_end);
        write_encrypted_segment(&storage_path, key, data, &aad)?;
        if let Err(error) = ready.database.append_terminal_segment(
            &segment_id,
            &terminal_id.to_string(),
            sequence_start,
            sequence_end,
            data.len(),
            &storage_path,
        ) {
            let _ = fs::remove_file(&storage_path);
            return Err(error.into());
        }
        let released = ready
            .database
            .prune_terminal_segments(&terminal_id.to_string(), MAX_TERMINAL_SCROLLBACK_BYTES)?;
        drop(state);
        remove_released_files(&released, &[directory])?;
        Ok(())
    }

    pub(crate) fn update_terminal_state(
        &self,
        terminal_id: TerminalId,
        state: &str,
    ) -> Result<(), StorageRuntimeError> {
        self.with_ready_database(|database| {
            database.update_terminal_state(&terminal_id.to_string(), state)
        })
    }

    pub(crate) fn start_session_run(
        &self,
        project_id: ProjectId,
        session_id: SessionId,
        run_id: RunId,
        process_id: u32,
        title: &str,
        invocation: &serde_json::Value,
    ) -> Result<(), StorageRuntimeError> {
        self.start_session_run_with_mode(
            project_id,
            session_id,
            run_id,
            process_id,
            title,
            invocation,
            IntegrationMode::Structured,
            "structured",
        )
    }

    pub(crate) fn start_tui_session_run(
        &self,
        project_id: ProjectId,
        session_id: SessionId,
        run_id: RunId,
        process_id: u32,
        title: &str,
        invocation: &serde_json::Value,
    ) -> Result<(), StorageRuntimeError> {
        self.start_session_run_with_mode(
            project_id,
            session_id,
            run_id,
            process_id,
            title,
            invocation,
            IntegrationMode::PtyTui,
            "pty_tui",
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn start_session_run_with_mode(
        &self,
        project_id: ProjectId,
        session_id: SessionId,
        run_id: RunId,
        process_id: u32,
        title: &str,
        invocation: &serde_json::Value,
        integration_mode: IntegrationMode,
        channel: &str,
    ) -> Result<(), StorageRuntimeError> {
        self.with_ready_session_store(|store| {
            store.upsert_session(
                session_id,
                project_id,
                AgentKind::Fake,
                integration_mode,
                SessionState::Starting,
                Some(title),
            )?;
            store.start_run(run_id, session_id, process_id, invocation, channel)
        })
    }

    pub(crate) fn persist_event(&self, event: &EventEnvelope) -> Result<(), StorageRuntimeError> {
        self.with_ready_session_store(|store| store.append_event(event))
    }

    pub(crate) fn update_session_state(
        &self,
        session_id: SessionId,
        state: SessionState,
    ) -> Result<(), StorageRuntimeError> {
        self.with_ready_session_store(|store| store.update_session_state(session_id, state))
    }

    pub(crate) fn finish_session_run(
        &self,
        session_id: SessionId,
        run_id: RunId,
        state: SessionState,
        exit: PersistedRunExit,
        recovery: Option<&serde_json::Value>,
    ) -> Result<(), StorageRuntimeError> {
        self.with_ready_session_store(|store| {
            store.update_session_state(session_id, state)?;
            store.finish_run(run_id, session_state_name(state), exit, recovery)
        })
    }

    pub(crate) fn persisted_events(
        &self,
        session_id: SessionId,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<EventEnvelope>, StorageRuntimeError> {
        self.with_ready_session_store(|store| store.load_events(session_id, after_sequence, limit))
    }

    pub(crate) fn persisted_session_metadata(
        &self,
        session_id: SessionId,
    ) -> Result<Option<PersistedSessionMetadata>, StorageRuntimeError> {
        self.with_ready_session_store(|store| store.session_metadata(session_id))
    }

    pub(crate) fn persisted_sessions(
        &self,
        project_id: ProjectId,
        limit: usize,
    ) -> Result<Vec<PersistedSessionSummary>, StorageRuntimeError> {
        self.with_ready_session_store(|store| store.list_sessions(project_id, limit))
    }

    pub(crate) fn persist_raw_protocol_capture(
        &self,
        session_id: SessionId,
        run_id: RunId,
        bytes: &[u8],
        observed_byte_count: u64,
        truncated: bool,
        completed: bool,
    ) -> Result<(), StorageRuntimeError> {
        let directory =
            prepare_private_directory(&self.paths.data_directory.join(RAW_SEGMENTS_DIRECTORY))?;
        let storage_path = directory.join(format!("{run_id}.capture"));
        let storage_path = storage_path.to_string_lossy();
        self.with_ready_session_store(|store| {
            store.upsert_raw_protocol_capture(
                session_id,
                run_id,
                bytes,
                observed_byte_count,
                truncated,
                completed,
                &storage_path,
            )
        })
    }

    pub(crate) fn persisted_raw_protocol_capture(
        &self,
        session_id: SessionId,
        run_id: RunId,
    ) -> Result<Option<PersistedRawProtocolCapture>, StorageRuntimeError> {
        self.with_ready_session_store(|store| store.raw_protocol_capture(session_id, run_id))
    }

    /// Runs one synchronous database operation while storage is ready. Callers
    /// execute these methods on a blocking worker so the state guard is never
    /// held across an async suspension point.
    fn with_ready_database<T>(
        &self,
        operation: impl FnOnce(&mut Database) -> Result<T, StorageError>,
    ) -> Result<T, StorageRuntimeError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| StorageRuntimeError::Unavailable)?;
        let StorageState::Ready(ready) = &mut *state else {
            return Err(StorageRuntimeError::Unavailable);
        };
        operation(&mut ready.database).map_err(StorageRuntimeError::from)
    }

    fn with_ready_session_store<T>(
        &self,
        operation: impl FnOnce(SessionStore<'_>) -> Result<T, SessionStoreError>,
    ) -> Result<T, StorageRuntimeError> {
        let state = self
            .state
            .lock()
            .map_err(|_| StorageRuntimeError::Unavailable)?;
        let StorageState::Ready(ready) = &*state else {
            return Err(StorageRuntimeError::Unavailable);
        };
        operation(SessionStore::new(&ready.database)).map_err(StorageRuntimeError::from)
    }

    /// Creates the current encrypted daily snapshot and applies bounded
    /// category retention while holding the daemon's single storage writer.
    pub(crate) fn maintain(&self) -> Result<(), StorageRuntimeError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| StorageRuntimeError::Unavailable)?;
        let StorageState::Ready(ready) = &mut *state else {
            return Ok(());
        };
        let Some(key) = ready.key.as_ref() else {
            return Ok(());
        };

        BackupStore::for_database(&self.paths.database)?
            .create_daily_snapshot(key, Utc::now().date_naive())?;

        let allowed_roots = prepare_retention_roots(&self.paths.data_directory)?;
        let mut policies = RetentionPolicies::default();
        // Capture creation is still explicit/default-off. Once a capture
        // exists, retain it within the category's bounded 10 MiB/7-day policy.
        policies.raw_protocol.enabled = true;
        let mut plan = plan_retention(ready.database.connection(), &policies, Utc::now())?;
        validate_retention_candidates(plan.candidates(), &allowed_roots)?;
        while !plan.is_empty() {
            let batch = execute_retention_batch(
                ready.database.connection(),
                &mut plan,
                maestro_storage::MAX_RETENTION_BATCH,
            )?;
            remove_released_files(&batch.released_storage_paths, &allowed_roots)?;
        }
        Ok(())
    }
}

fn session_state_name(state: SessionState) -> &'static str {
    match state {
        SessionState::Created => "created",
        SessionState::Starting => "starting",
        SessionState::Ready => "ready",
        SessionState::Running => "running",
        SessionState::AwaitingPermission => "awaiting_permission",
        SessionState::AwaitingUserInput => "awaiting_user_input",
        SessionState::Background => "background",
        SessionState::Interrupting => "interrupting",
        SessionState::Completed => "completed",
        SessionState::Stopped => "stopped",
        SessionState::Failed => "failed",
        SessionState::Interrupted => "interrupted",
        SessionState::Recoverable => "recoverable",
        SessionState::Incompatible => "incompatible",
    }
}

fn initialize_persistent(paths: &DaemonPaths) -> StorageState {
    if paths.database_key_envelope.exists() {
        return StorageState::PassphraseRequired(StorageUnlockMode::Unlock);
    }

    match OsKeyStore::default().load_or_create() {
        Ok(key) => open_persistent_database(paths, key)
            .map_or(StorageState::Unavailable, StorageState::Ready),
        Err(_) if cfg!(target_os = "linux") && !paths.database.exists() => {
            StorageState::PassphraseRequired(StorageUnlockMode::Create)
        }
        Err(_) => StorageState::Unavailable,
    }
}

fn open_persistent_database(
    paths: &DaemonPaths,
    key: DatabaseKey,
) -> Result<ReadyStorage, StorageRuntimeError> {
    if paths.database.exists() {
        BackupStore::for_database(&paths.database)?
            .create_daily_snapshot(&key, Utc::now().date_naive())?;
    }
    let database = Database::open(&paths.database, &key)?;
    let StorageState::Ready(ready) = ready_database(database, Some(key))? else {
        unreachable!("ready_database always returns ready storage");
    };
    Ok(ready)
}

fn ready_database(
    mut database: Database,
    key: Option<DatabaseKey>,
) -> Result<StorageState, StorageError> {
    database.recover_interrupted_work()?;
    Ok(StorageState::Ready(ReadyStorage { database, key }))
}

fn prepare_retention_roots(data_directory: &Path) -> Result<Vec<PathBuf>, StorageRuntimeError> {
    [
        TERMINAL_SEGMENTS_DIRECTORY,
        RAW_SEGMENTS_DIRECTORY,
        DEBUG_LOGS_DIRECTORY,
    ]
    .into_iter()
    .map(|name| prepare_private_directory(&data_directory.join(name)))
    .collect()
}

fn prepare_private_directory(path: &Path) -> Result<PathBuf, StorageRuntimeError> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(StorageRuntimeError::UnsafeRetentionPath(path.to_path_buf()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(fs::canonicalize(path)?)
}

fn validate_retention_candidates(
    candidates: &[maestro_storage::RetentionRecord],
    allowed_roots: &[PathBuf],
) -> Result<(), StorageRuntimeError> {
    for candidate in candidates {
        validate_released_path(&candidate.storage_path, allowed_roots)?;
    }
    Ok(())
}

fn validate_released_path(
    path: &Path,
    allowed_roots: &[PathBuf],
) -> Result<(), StorageRuntimeError> {
    if !path.is_absolute() || path.file_name().is_none() {
        return Err(StorageRuntimeError::UnsafeRetentionPath(path.to_path_buf()));
    }
    let Some(parent) = path.parent() else {
        return Err(StorageRuntimeError::UnsafeRetentionPath(path.to_path_buf()));
    };
    let canonical_parent = fs::canonicalize(parent)
        .map_err(|_| StorageRuntimeError::UnsafeRetentionPath(path.to_path_buf()))?;
    if !allowed_roots.contains(&canonical_parent) {
        return Err(StorageRuntimeError::UnsafeRetentionPath(path.to_path_buf()));
    }
    Ok(())
}

fn remove_released_files(
    paths: &[PathBuf],
    allowed_roots: &[PathBuf],
) -> Result<(), StorageRuntimeError> {
    for path in paths {
        validate_released_path(path, allowed_roots)?;
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_file() || metadata.file_type().is_symlink() => {
                fs::remove_file(path)?;
            }
            Ok(_) => return Err(StorageRuntimeError::UnsafeRetentionPath(path.clone())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn terminal_segment_aad(
    terminal_id: TerminalId,
    sequence_start: u64,
    sequence_end: u64,
) -> Vec<u8> {
    format!(
        "{}\0{terminal_id}\0{sequence_start}\0{sequence_end}",
        String::from_utf8_lossy(TERMINAL_SEGMENT_AAD)
    )
    .into_bytes()
}

fn write_encrypted_segment(
    path: &Path,
    key: &DatabaseKey,
    plaintext: &[u8],
    aad: &[u8],
) -> Result<(), StorageRuntimeError> {
    let cipher = terminal_segment_cipher(key)?;
    let mut nonce = [0_u8; 24];
    rand::rng().fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| StorageRuntimeError::SegmentEncryption)?;
    let parent = path
        .parent()
        .ok_or_else(|| StorageRuntimeError::UnsafeRetentionPath(path.to_path_buf()))?;
    let temporary = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let publication = (|| -> Result<(), StorageRuntimeError> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(TERMINAL_SEGMENT_MAGIC)?;
        file.write_all(&nonce)?;
        file.write_all(&ciphertext)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        FileSync::sync_directory(parent)?;
        Ok(())
    })();
    if publication.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    publication
}

fn terminal_segment_cipher(key: &DatabaseKey) -> Result<XChaCha20Poly1305, StorageRuntimeError> {
    let mut derivation = Sha256::new();
    derivation.update(TERMINAL_SEGMENT_AAD);
    derivation.update([0]);
    derivation.update(key.expose());
    let derived = Zeroizing::new(<[u8; 32]>::from(derivation.finalize()));
    XChaCha20Poly1305::new_from_slice(derived.as_ref())
        .map_err(|_| StorageRuntimeError::SegmentEncryption)
}

struct FileSync;

impl FileSync {
    fn sync_directory(path: &Path) -> Result<(), std::io::Error> {
        fs::File::open(path)?.sync_all()
    }
}

#[derive(Debug, Error)]
pub(crate) enum StorageRuntimeError {
    #[error("encrypted storage is unavailable")]
    Unavailable,
    #[error("database key operation failed")]
    Key(#[from] KeyStoreError),
    #[error("database operation failed")]
    Storage(#[from] StorageError),
    #[error("session persistence operation failed")]
    Session(#[from] SessionStoreError),
    #[error("encrypted database backup failed")]
    Backup(#[from] BackupError),
    #[error("storage retention failed")]
    Retention(#[from] RetentionError),
    #[error("storage maintenance I/O failed")]
    Io(#[from] std::io::Error),
    #[error("storage retention path is outside Maestro-owned directories: {0}")]
    UnsafeRetentionPath(PathBuf),
    #[error("terminal scrollback encryption failed")]
    SegmentEncryption,
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        sync::{Arc, Barrier, Mutex},
    };

    use chacha20poly1305::{
        XNonce,
        aead::{Aead, Payload},
    };
    use maestro_domain::{ProjectId, TerminalId};
    use maestro_protocol::{StorageStatus, StorageUnlockMode};
    use maestro_storage::{BackupStore, Database, DatabaseKey};

    use super::{
        MAX_TERMINAL_SCROLLBACK_BYTES, StorageRuntime, StorageState, TERMINAL_SEGMENT_MAGIC,
        prepare_private_directory, ready_database, terminal_segment_aad, terminal_segment_cipher,
    };
    use crate::DaemonPaths;

    #[test]
    fn private_directory_creation_is_concurrently_idempotent() {
        const CREATORS: usize = 16;

        let temporary = tempfile::tempdir().expect("temporary directory");
        let directory = Arc::new(temporary.path().join("concurrent-private-directory"));
        let barrier = Arc::new(Barrier::new(CREATORS));
        let creators = (0..CREATORS)
            .map(|_| {
                let directory = Arc::clone(&directory);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    prepare_private_directory(&directory)
                })
            })
            .collect::<Vec<_>>();

        let expected = std::fs::canonicalize(temporary.path())
            .expect("temporary root canonicalizes")
            .join("concurrent-private-directory");
        for creator in creators {
            assert_eq!(
                creator
                    .join()
                    .expect("private-directory creator does not panic")
                    .expect("concurrent private-directory creation succeeds"),
                expected
            );
        }
    }
    #[test]
    fn isolated_runtime_is_ready_and_migrated() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let runtime = StorageRuntime::initialize(DaemonPaths::isolated(temporary.path()))
            .expect("isolated storage initializes");

        assert_eq!(runtime.snapshot(), (StorageStatus::Ready, Some(3)));
    }

    #[test]
    fn passphrase_storage_creates_then_requires_the_same_passphrase() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = DaemonPaths::isolated(temporary.path());
        paths.prepare().expect("paths prepare");
        let runtime = StorageRuntime {
            paths: paths.clone(),
            state: Mutex::new(StorageState::PassphraseRequired(StorageUnlockMode::Create)),
        };
        runtime
            .unlock("foundation-passphrase".to_owned())
            .expect("storage creates");
        assert_eq!(runtime.snapshot(), (StorageStatus::Ready, Some(3)));
        drop(runtime);

        let runtime = StorageRuntime {
            paths,
            state: Mutex::new(StorageState::PassphraseRequired(StorageUnlockMode::Unlock)),
        };
        assert!(runtime.unlock("wrong-passphrase".to_owned()).is_err());
        assert_eq!(
            runtime.snapshot(),
            (
                StorageStatus::PassphraseRequired {
                    mode: StorageUnlockMode::Unlock,
                },
                None,
            )
        );
        runtime
            .unlock("foundation-passphrase".to_owned())
            .expect("storage unlocks");
        assert_eq!(runtime.snapshot(), (StorageStatus::Ready, Some(3)));
    }

    #[test]
    fn persistent_maintenance_creates_an_encrypted_daily_snapshot() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = DaemonPaths::isolated(temporary.path());
        paths.prepare().expect("paths prepare");
        let key = DatabaseKey::generate();
        let database = Database::open(&paths.database, &key).expect("database opens");
        let backup_directory = BackupStore::for_database(&paths.database)
            .expect("backup store")
            .directory()
            .to_path_buf();
        let runtime = StorageRuntime {
            paths,
            state: Mutex::new(
                ready_database(database, Some(key)).expect("ready persistent database"),
            ),
        };

        runtime.maintain().expect("maintenance succeeds");

        let snapshots = std::fs::read_dir(backup_directory)
            .expect("backup directory exists")
            .collect::<Result<Vec<_>, _>>()
            .expect("backup directory reads");
        assert_eq!(
            snapshots
                .iter()
                .filter(|entry| entry
                    .path()
                    .extension()
                    .is_some_and(|value| value == "sqlcipher"))
                .count(),
            1
        );
    }

    #[test]
    fn project_favorite_and_window_layout_survive_runtime_restart() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = DaemonPaths::isolated(temporary.path());
        paths.prepare().expect("paths prepare");
        let key = DatabaseKey::generate();
        let database = Database::open(&paths.database, &key).expect("database opens");
        let runtime = StorageRuntime {
            paths: paths.clone(),
            state: Mutex::new(
                ready_database(database, Some(key.clone())).expect("ready persistent database"),
            ),
        };
        let roots = vec!["/workspace/maestro".to_owned()];

        runtime
            .upsert_project("project-1", "Maestro", &roots)
            .expect("project persists");
        runtime
            .set_project_favorite("project-1", true)
            .expect("favorite persists");
        runtime
            .save_window_layout("project-1", "main", r#"{"version":1,"leftWidth":280}"#)
            .expect("layout persists");
        drop(runtime);

        let database = Database::open(&paths.database, &key).expect("database reopens");
        let restarted = StorageRuntime {
            paths,
            state: Mutex::new(
                ready_database(database, Some(key)).expect("restarted database becomes ready"),
            ),
        };
        let projects = restarted.recent_projects(10).expect("projects restore");
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].id, "project-1");
        assert_eq!(projects[0].canonical_roots, roots);
        assert!(projects[0].favorite);
        assert_eq!(
            restarted
                .window_layout("project-1", "main")
                .expect("layout restores")
                .as_deref(),
            Some(r#"{"version":1,"leftWidth":280}"#)
        );
    }

    #[test]
    fn project_and_layout_operations_fail_closed_while_storage_is_locked() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let runtime = StorageRuntime {
            paths: DaemonPaths::isolated(temporary.path()),
            state: Mutex::new(StorageState::PassphraseRequired(StorageUnlockMode::Unlock)),
        };

        assert!(matches!(
            runtime.upsert_project("project-1", "Maestro", &["/workspace".to_owned()]),
            Err(super::StorageRuntimeError::Unavailable)
        ));
        assert!(matches!(
            runtime.recent_projects(10),
            Err(super::StorageRuntimeError::Unavailable)
        ));
        assert!(matches!(
            runtime.set_project_favorite("project-1", true),
            Err(super::StorageRuntimeError::Unavailable)
        ));
        assert!(matches!(
            runtime.save_window_layout("project-1", "main", "{}"),
            Err(super::StorageRuntimeError::Unavailable)
        ));
        assert!(matches!(
            runtime.window_layout("project-1", "main"),
            Err(super::StorageRuntimeError::Unavailable)
        ));
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "the encryption evidence keeps retention, marker scanning, and key checks together"
    )]
    fn terminal_scrollback_is_encrypted_and_pruned_per_terminal() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = DaemonPaths::isolated(temporary.path());
        paths.prepare().expect("paths prepare");
        let key = DatabaseKey::generate();
        let database = Database::open(&paths.database, &key).expect("database opens");
        let runtime = StorageRuntime {
            paths,
            state: Mutex::new(
                ready_database(database, Some(key.clone())).expect("ready persistent database"),
            ),
        };
        let project_id = ProjectId::new();
        let terminal_id = TerminalId::new();
        runtime
            .upsert_project(
                &project_id.to_string(),
                "Terminal persistence",
                &[temporary.path().to_string_lossy().into_owned()],
            )
            .expect("project persists");
        runtime
            .register_terminal(project_id, terminal_id, "shell", "Shell")
            .expect("terminal registers");

        let mut plaintext = vec![b'x'; 1024 * 1024];
        plaintext[..32].copy_from_slice(b"terminal-secret-marker-123456789");
        for sequence in 1..=11 {
            runtime
                .persist_terminal_segment(terminal_id, sequence, sequence, &plaintext)
                .expect("encrypted segment persists");
        }

        let (stored_bytes, count, storage_path, sequence_start, sequence_end) = {
            let state = runtime.state.lock().expect("storage state");
            let StorageState::Ready(ready) = &*state else {
                panic!("storage is ready");
            };
            let (stored_bytes, count) = ready
                .database
                .connection()
                .query_row(
                    "SELECT COALESCE(SUM(byte_count), 0), COUNT(*)
                     FROM terminal_segments WHERE terminal_tab_id = ?1",
                    [terminal_id.to_string()],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .expect("retained totals query");
            let latest = ready
                .database
                .connection()
                .query_row(
                    "SELECT storage_path, sequence_start, sequence_end
                     FROM terminal_segments WHERE terminal_tab_id = ?1
                     ORDER BY sequence_end DESC LIMIT 1",
                    [terminal_id.to_string()],
                    |row| {
                        Ok((
                            PathBuf::from(row.get::<_, String>(0)?),
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .expect("latest segment query");
            (
                u64::try_from(stored_bytes).expect("non-negative stored bytes"),
                u64::try_from(count).expect("non-negative segment count"),
                latest.0,
                u64::try_from(latest.1).expect("non-negative start sequence"),
                u64::try_from(latest.2).expect("non-negative end sequence"),
            )
        };
        assert_eq!(stored_bytes, MAX_TERMINAL_SCROLLBACK_BYTES);
        assert_eq!(count, 10);

        let encrypted = std::fs::read(storage_path).expect("encrypted segment reads");
        assert!(encrypted.starts_with(TERMINAL_SEGMENT_MAGIC));
        assert!(
            !encrypted
                .windows(b"terminal-secret-marker".len())
                .any(|window| window == b"terminal-secret-marker")
        );
        let nonce_start = TERMINAL_SEGMENT_MAGIC.len();
        let ciphertext_start = nonce_start + 24;
        let aad = terminal_segment_aad(terminal_id, sequence_start, sequence_end);
        let correct = terminal_segment_cipher(&key).expect("valid derived key");
        let decrypted = correct
            .decrypt(
                XNonce::from_slice(&encrypted[nonce_start..ciphertext_start]),
                Payload {
                    msg: &encrypted[ciphertext_start..],
                    aad: &aad,
                },
            )
            .expect("correct key decrypts");
        assert_eq!(decrypted, plaintext);
        let wrong_key = DatabaseKey::generate();
        let wrong = terminal_segment_cipher(&wrong_key).expect("valid derived key");
        assert!(
            wrong
                .decrypt(
                    XNonce::from_slice(&encrypted[nonce_start..ciphertext_start]),
                    Payload {
                        msg: &encrypted[ciphertext_start..],
                        aad: &aad,
                    },
                )
                .is_err()
        );
    }
}
