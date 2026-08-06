//! SQLCipher-backed local persistence. Vendor-owned credentials and session
//! databases are deliberately outside this crate's ownership.

mod backup;
mod database;
mod key;
mod retention;
mod session;

pub use backup::{BackupError, BackupResult, BackupRotation, BackupStore, verify_snapshot};
pub use database::{Database, PersistedProject, RecoverySummary, StorageError};
pub use key::{DatabaseKey, DatabaseKeyStore, KeyStoreError, OsKeyStore, PassphraseKeyStore};
pub use retention::{
    MAX_RETENTION_BATCH, RetentionBatch, RetentionCategory, RetentionError, RetentionPlan,
    RetentionPolicies, RetentionPolicy, RetentionRecord, execute_retention_batch, plan_retention,
};
pub use session::{
    PersistedRawProtocolCapture, PersistedRunExit, PersistedSessionMetadata,
    PersistedSessionSummary, SessionStore, SessionStoreError,
};
