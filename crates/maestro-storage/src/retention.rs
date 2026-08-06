use std::{collections::HashMap, path::PathBuf};

use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, OptionalExtension};
use thiserror::Error;

pub const MAX_RETENTION_BATCH: usize = 256;
const DEFAULT_TERMINAL_BYTES: u64 = 10 * 1024 * 1024;
const DEFAULT_RAW_BYTES: u64 = 10 * 1024 * 1024;
const DEFAULT_DEBUG_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RetentionCategory {
    Terminal,
    RawProtocol,
    DebugLog,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RetentionPolicy {
    pub enabled: bool,
    pub max_bytes_per_owner: Option<u64>,
    pub max_age: Option<Duration>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RetentionPolicies {
    pub terminal: RetentionPolicy,
    pub raw_protocol: RetentionPolicy,
    pub debug_log: RetentionPolicy,
}

impl Default for RetentionPolicies {
    fn default() -> Self {
        Self {
            terminal: RetentionPolicy {
                enabled: true,
                max_bytes_per_owner: Some(DEFAULT_TERMINAL_BYTES),
                max_age: None,
            },
            raw_protocol: RetentionPolicy {
                enabled: false,
                max_bytes_per_owner: Some(DEFAULT_RAW_BYTES),
                max_age: Some(Duration::days(7)),
            },
            debug_log: RetentionPolicy {
                enabled: true,
                max_bytes_per_owner: Some(DEFAULT_DEBUG_BYTES),
                max_age: Some(Duration::days(14)),
            },
        }
    }
}

impl RetentionPolicies {
    fn policy(&self, category: RetentionCategory) -> &RetentionPolicy {
        match category {
            RetentionCategory::Terminal => &self.terminal,
            RetentionCategory::RawProtocol => &self.raw_protocol,
            RetentionCategory::DebugLog => &self.debug_log,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RetentionRecord {
    pub id: String,
    pub category: RetentionCategory,
    pub owner_id: String,
    pub created_at: DateTime<Utc>,
    pub byte_count: u64,
    pub storage_path: PathBuf,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RetentionPlan {
    candidates: Vec<RetentionRecord>,
}

impl RetentionPlan {
    /// Plans category-specific deletion without including normalized events.
    ///
    /// Disabled categories are fully selected. Enabled categories independently
    /// apply age and per-owner byte limits, deleting oldest records first.
    pub fn from_records(
        mut records: Vec<RetentionRecord>,
        policies: &RetentionPolicies,
        now: DateTime<Utc>,
    ) -> Self {
        records.sort_by(|left, right| {
            (left.category, left.id.as_str()).cmp(&(right.category, right.id.as_str()))
        });
        records.dedup_by(|left, right| left.category == right.category && left.id == right.id);

        let mut selected = Vec::new();
        for category in [
            RetentionCategory::Terminal,
            RetentionCategory::RawProtocol,
            RetentionCategory::DebugLog,
        ] {
            let policy = policies.policy(category);
            let mut category_records: Vec<RetentionRecord> = records
                .iter()
                .filter(|record| record.category == category)
                .cloned()
                .collect();
            if !policy.enabled {
                selected.extend(category_records);
                continue;
            }

            let cutoff = policy.max_age.map(|max_age| now - max_age);
            let mut retained = Vec::new();
            for record in category_records.drain(..) {
                if cutoff.is_some_and(|cutoff| record.created_at < cutoff) {
                    selected.push(record);
                } else {
                    retained.push(record);
                }
            }

            if let Some(maximum) = policy.max_bytes_per_owner {
                let mut by_owner: HashMap<String, Vec<RetentionRecord>> = HashMap::new();
                for record in retained {
                    by_owner
                        .entry(record.owner_id.clone())
                        .or_default()
                        .push(record);
                }
                for owner_records in by_owner.values_mut() {
                    owner_records.sort_by(|left, right| {
                        (left.created_at, left.id.as_str())
                            .cmp(&(right.created_at, right.id.as_str()))
                    });
                    let mut total = owner_records
                        .iter()
                        .fold(0_u64, |sum, record| sum.saturating_add(record.byte_count));
                    for record in owner_records.iter() {
                        if total <= maximum {
                            break;
                        }
                        total = total.saturating_sub(record.byte_count);
                        selected.push(record.clone());
                    }
                }
            }
        }

        selected.sort_by(|left, right| {
            (left.category, left.created_at, left.id.as_str()).cmp(&(
                right.category,
                right.created_at,
                right.id.as_str(),
            ))
        });
        selected.dedup_by(|left, right| left.category == right.category && left.id == right.id);
        Self {
            candidates: selected,
        }
    }

    pub fn candidates(&self) -> &[RetentionRecord] {
        &self.candidates
    }

    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }

    pub fn len(&self) -> usize {
        self.candidates.len()
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct RetentionBatch {
    pub metadata_rows_deleted: usize,
    pub released_storage_paths: Vec<PathBuf>,
    pub remaining_candidates: usize,
}

/// Loads terminal, raw-protocol, and optional debug-log metadata and plans
/// category-specific retention. Normalized `events` are intentionally never
/// loaded as candidates.
///
/// The optional debug table contract is:
/// `debug_logs(id, component, created_at, byte_count, storage_path)`.
/// Until its migration is installed, debug planning safely returns no rows.
///
/// # Errors
///
/// Returns [`RetentionError`] for malformed timestamps, negative byte counts,
/// or database errors.
pub fn plan_retention(
    connection: &Connection,
    policies: &RetentionPolicies,
    now: DateTime<Utc>,
) -> Result<RetentionPlan, RetentionError> {
    let mut records = Vec::new();
    load_records(
        connection,
        RetentionCategory::Terminal,
        "SELECT id, terminal_tab_id, created_at, byte_count, storage_path \
         FROM terminal_segments",
        &mut records,
    )?;
    load_records(
        connection,
        RetentionCategory::RawProtocol,
        "SELECT id, session_id, started_at, byte_count, storage_path \
         FROM raw_segments",
        &mut records,
    )?;
    if table_exists(connection, "debug_logs")? {
        load_records(
            connection,
            RetentionCategory::DebugLog,
            "SELECT id, component, created_at, byte_count, storage_path FROM debug_logs",
            &mut records,
        )?;
    }
    Ok(RetentionPlan::from_records(records, policies, now))
}

/// Executes at most 256 planned metadata deletions in one transaction.
///
/// The plan is drained only after commit, so failures are retryable. Replaying
/// a committed candidate is idempotent because deletion is keyed by immutable
/// IDs. Returned paths are advisory: the daemon must separately validate them
/// against its Maestro-owned segment directory before removing files.
/// The daemon's single-writer queue must ensure no transaction is already open
/// on `connection` when this function is called.
///
/// # Errors
///
/// Returns [`RetentionError`] when `requested_limit` is zero or the bounded
/// transaction cannot commit.
pub fn execute_retention_batch(
    connection: &Connection,
    plan: &mut RetentionPlan,
    requested_limit: usize,
) -> Result<RetentionBatch, RetentionError> {
    if requested_limit == 0 {
        return Err(RetentionError::ZeroBatchSize);
    }
    let batch_size = requested_limit.min(MAX_RETENTION_BATCH).min(plan.len());
    if batch_size == 0 {
        return Ok(RetentionBatch::default());
    }
    let batch = plan.candidates[..batch_size].to_vec();
    let transaction = connection.unchecked_transaction()?;
    let mut deleted = 0;
    let mut released = Vec::new();

    for candidate in &batch {
        let affected = match candidate.category {
            RetentionCategory::Terminal => transaction.execute(
                "DELETE FROM terminal_segments WHERE id = ?1",
                [&candidate.id],
            )?,
            RetentionCategory::RawProtocol => {
                let affected = transaction
                    .execute("DELETE FROM raw_segments WHERE id = ?1", [&candidate.id])?;
                if affected > 0 {
                    transaction.execute(
                        "UPDATE events SET raw_segment_reference = NULL \
                         WHERE raw_segment_reference = ?1",
                        [candidate.storage_path.to_string_lossy().as_ref()],
                    )?;
                }
                affected
            }
            RetentionCategory::DebugLog => {
                transaction.execute("DELETE FROM debug_logs WHERE id = ?1", [&candidate.id])?
            }
        };
        if affected > 0 {
            deleted += affected;
            released.push(candidate.storage_path.clone());
        }
    }

    transaction.commit()?;
    plan.candidates.drain(..batch_size);
    Ok(RetentionBatch {
        metadata_rows_deleted: deleted,
        released_storage_paths: released,
        remaining_candidates: plan.len(),
    })
}

fn load_records(
    connection: &Connection,
    category: RetentionCategory,
    sql: &str,
    output: &mut Vec<RetentionRecord>,
) -> Result<(), RetentionError> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    for row in rows {
        let (id, owner_id, timestamp, byte_count, storage_path) = row?;
        if byte_count < 0 {
            return Err(RetentionError::NegativeByteCount { id, byte_count });
        }
        let created_at = DateTime::parse_from_rfc3339(&timestamp)
            .map_err(|source| RetentionError::InvalidTimestamp {
                value: timestamp,
                source,
            })?
            .with_timezone(&Utc);
        output.push(RetentionRecord {
            id,
            category,
            owner_id,
            created_at,
            byte_count: u64::try_from(byte_count).unwrap_or_default(),
            storage_path: storage_path.into(),
        });
    }
    Ok(())
}

fn table_exists(connection: &Connection, name: &str) -> Result<bool, rusqlite::Error> {
    connection
        .query_row(
            "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            [name],
            |_| Ok(true),
        )
        .optional()
        .map(Option::unwrap_or_default)
}

#[derive(Debug, Error)]
pub enum RetentionError {
    #[error("retention metadata {id} has negative byte count {byte_count}")]
    NegativeByteCount { id: String, byte_count: i64 },
    #[error("retention metadata timestamp {value:?} is invalid: {source}")]
    InvalidTimestamp {
        value: String,
        source: chrono::ParseError,
    },
    #[error("retention batch size must be greater than zero")]
    ZeroBatchSize,
    #[error("retention database operation failed: {0}")]
    Sql(#[from] rusqlite::Error),
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone, Utc};
    use rusqlite::Connection;

    use super::{
        MAX_RETENTION_BATCH, RetentionCategory, RetentionPlan, RetentionPolicies, RetentionPolicy,
        RetentionRecord, execute_retention_batch, plan_retention,
    };

    #[test]
    fn raw_defaults_off_and_category_limits_are_independent() {
        let now = Utc.with_ymd_and_hms(2026, 8, 5, 12, 0, 0).unwrap();
        let records = vec![
            record("terminal", RetentionCategory::Terminal, "tab", now, 5),
            record("raw-old", RetentionCategory::RawProtocol, "session", now, 6),
            record(
                "raw-new",
                RetentionCategory::RawProtocol,
                "session",
                now + Duration::seconds(1),
                6,
            ),
            record("debug", RetentionCategory::DebugLog, "daemon", now, 5),
        ];

        let default_plan =
            RetentionPlan::from_records(records.clone(), &RetentionPolicies::default(), now);
        assert_eq!(
            default_plan
                .candidates()
                .iter()
                .map(|candidate| candidate.id.as_str())
                .collect::<Vec<_>>(),
            vec!["raw-old", "raw-new"]
        );

        let mut enabled = RetentionPolicies::default();
        enabled.raw_protocol = RetentionPolicy {
            enabled: true,
            max_bytes_per_owner: Some(10),
            max_age: None,
        };
        let enabled_plan = RetentionPlan::from_records(records, &enabled, now);
        assert_eq!(enabled_plan.len(), 1);
        assert_eq!(enabled_plan.candidates()[0].id, "raw-old");
    }

    #[test]
    fn planner_loads_all_metadata_categories_but_never_normalized_history() {
        let now = Utc.with_ymd_and_hms(2026, 8, 5, 12, 0, 0).unwrap();
        let connection = retention_fixture();
        seed_all_categories(&connection, now);
        let mut policies = RetentionPolicies::default();
        policies.terminal.max_bytes_per_owner = Some(0);
        policies.debug_log.max_age = Some(Duration::hours(1));

        let plan = plan_retention(&connection, &policies, now).expect("plan loads");
        assert_eq!(plan.len(), 3);
        assert!(
            plan.candidates()
                .iter()
                .any(|candidate| candidate.category == RetentionCategory::Terminal)
        );
        assert!(
            plan.candidates()
                .iter()
                .any(|candidate| candidate.category == RetentionCategory::RawProtocol)
        );
        assert!(
            plan.candidates()
                .iter()
                .any(|candidate| candidate.category == RetentionCategory::DebugLog)
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM events", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn planner_tolerates_debug_metadata_table_not_yet_migrated() {
        let now = Utc.with_ymd_and_hms(2026, 8, 5, 12, 0, 0).unwrap();
        let connection = retention_fixture();
        connection.execute_batch("DROP TABLE debug_logs").unwrap();
        let plan = plan_retention(&connection, &RetentionPolicies::default(), now)
            .expect("missing optional debug metadata is safe");
        assert!(plan.is_empty());
    }

    #[test]
    fn bounded_execution_preserves_events_and_is_idempotent() {
        let now = Utc.with_ymd_and_hms(2026, 8, 5, 12, 0, 0).unwrap();
        let connection = retention_fixture();
        seed_all_categories(&connection, now);
        let mut plan =
            plan_retention(&connection, &RetentionPolicies::default(), now).expect("default plan");
        assert_eq!(plan.len(), 1);
        let replay = plan.clone();

        let batch = execute_retention_batch(&connection, &mut plan, 1).expect("batch commits");
        assert_eq!(batch.metadata_rows_deleted, 1);
        assert_eq!(
            batch.released_storage_paths,
            vec![std::path::PathBuf::from("raw.bin")]
        );
        assert!(plan.is_empty());
        assert_eq!(
            connection
                .query_row("SELECT payload_json FROM events", [], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap(),
            "normalized-history"
        );
        assert_eq!(
            connection
                .query_row("SELECT raw_segment_reference FROM events", [], |row| {
                    row.get::<_, Option<String>>(0)
                })
                .unwrap(),
            None
        );
        assert_eq!(table_count(&connection, "terminal_segments"), 1);
        assert_eq!(table_count(&connection, "debug_logs"), 1);

        let mut replay = replay;
        let repeated = execute_retention_batch(&connection, &mut replay, 1)
            .expect("replayed delete is harmless");
        assert_eq!(repeated.metadata_rows_deleted, 0);
        assert!(repeated.released_storage_paths.is_empty());
    }

    #[test]
    fn failed_batch_rolls_back_and_leaves_plan_retryable() {
        let now = Utc.with_ymd_and_hms(2026, 8, 5, 12, 0, 0).unwrap();
        let connection = retention_fixture();
        seed_all_categories(&connection, now);
        connection
            .execute_batch("DROP TABLE debug_logs")
            .expect("debug table removed");
        let records = vec![
            record("terminal", RetentionCategory::Terminal, "tab", now, 1),
            record("debug", RetentionCategory::DebugLog, "daemon", now, 1),
        ];
        let policies = RetentionPolicies {
            terminal: delete_all_enabled_policy(),
            raw_protocol: delete_all_enabled_policy(),
            debug_log: delete_all_enabled_policy(),
        };
        let mut plan = RetentionPlan::from_records(records, &policies, now);

        assert!(execute_retention_batch(&connection, &mut plan, 2).is_err());
        assert_eq!(plan.len(), 2);
        assert_eq!(table_count(&connection, "terminal_segments"), 1);

        connection
            .execute_batch(
                "CREATE TABLE debug_logs (
                    id TEXT PRIMARY KEY,
                    component TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    byte_count INTEGER NOT NULL,
                    storage_path TEXT NOT NULL
                 );
                 INSERT INTO debug_logs VALUES
                    ('debug', 'daemon', '2026-08-05T12:00:00Z', 1, 'debug.log');",
            )
            .expect("debug metadata restored");
        let batch = execute_retention_batch(&connection, &mut plan, 2).expect("retry commits");
        assert_eq!(batch.metadata_rows_deleted, 2);
        assert!(plan.is_empty());
    }

    #[test]
    fn transaction_size_is_hard_bounded() {
        let now = Utc.with_ymd_and_hms(2026, 8, 5, 12, 0, 0).unwrap();
        let mut connection = retention_fixture();
        let transaction = connection.transaction().unwrap();
        for index in 0..300 {
            transaction
                .execute(
                    "INSERT INTO terminal_segments VALUES (?1, 'tab', ?2, ?2, 1, ?3, ?4)",
                    rusqlite::params![
                        format!("terminal-{index}"),
                        index,
                        format!("terminal-{index}.bin"),
                        now.to_rfc3339()
                    ],
                )
                .unwrap();
        }
        transaction.commit().unwrap();
        let records = (0..300)
            .map(|index| {
                record(
                    &format!("terminal-{index}"),
                    RetentionCategory::Terminal,
                    "tab",
                    now,
                    1,
                )
            })
            .collect();
        let policies = RetentionPolicies {
            terminal: delete_all_enabled_policy(),
            raw_protocol: delete_all_enabled_policy(),
            debug_log: delete_all_enabled_policy(),
        };
        let mut plan = RetentionPlan::from_records(records, &policies, now);

        let batch = execute_retention_batch(&connection, &mut plan, usize::MAX)
            .expect("bounded batch commits");
        assert_eq!(batch.metadata_rows_deleted, MAX_RETENTION_BATCH);
        assert_eq!(batch.remaining_candidates, 300 - MAX_RETENTION_BATCH);
        assert_eq!(
            table_count(&connection, "terminal_segments"),
            300 - MAX_RETENTION_BATCH
        );
    }

    fn record(
        id: &str,
        category: RetentionCategory,
        owner_id: &str,
        created_at: chrono::DateTime<Utc>,
        byte_count: u64,
    ) -> RetentionRecord {
        RetentionRecord {
            id: id.into(),
            category,
            owner_id: owner_id.into(),
            created_at,
            byte_count,
            storage_path: format!("{id}.bin").into(),
        }
    }

    fn delete_all_enabled_policy() -> RetentionPolicy {
        RetentionPolicy {
            enabled: true,
            max_bytes_per_owner: Some(0),
            max_age: None,
        }
    }

    fn retention_fixture() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE terminal_segments (
                    id TEXT PRIMARY KEY,
                    terminal_tab_id TEXT NOT NULL,
                    sequence_start INTEGER NOT NULL,
                    sequence_end INTEGER NOT NULL,
                    byte_count INTEGER NOT NULL,
                    storage_path TEXT NOT NULL,
                    created_at TEXT NOT NULL
                 );
                 CREATE TABLE raw_segments (
                    id TEXT PRIMARY KEY,
                    session_id TEXT NOT NULL,
                    started_at TEXT NOT NULL,
                    ended_at TEXT,
                    byte_count INTEGER NOT NULL,
                    storage_path TEXT NOT NULL
                 );
                 CREATE TABLE events (
                    id TEXT PRIMARY KEY,
                    payload_json TEXT NOT NULL,
                    raw_segment_reference TEXT
                 );
                 CREATE TABLE debug_logs (
                    id TEXT PRIMARY KEY,
                    component TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    byte_count INTEGER NOT NULL,
                    storage_path TEXT NOT NULL
                 );",
            )
            .unwrap();
        connection
    }

    fn seed_all_categories(connection: &Connection, now: chrono::DateTime<Utc>) {
        connection
            .execute(
                "INSERT INTO terminal_segments VALUES
                 ('terminal', 'tab', 0, 1, 1, 'terminal.bin', ?1)",
                [now.to_rfc3339()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO raw_segments VALUES
                 ('raw', 'session', ?1, NULL, 1, 'raw.bin')",
                [now.to_rfc3339()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO events VALUES
                 ('event', 'normalized-history', 'raw.bin')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO debug_logs VALUES
                 ('debug', 'daemon', ?1, 1, 'debug.log')",
                [(now - Duration::hours(2)).to_rfc3339()],
            )
            .unwrap();
    }

    fn table_count(connection: &Connection, table: &str) -> usize {
        let sql = format!("SELECT count(*) FROM {table}");
        let count = connection
            .query_row(&sql, [], |row| row.get::<_, i64>(0))
            .unwrap();
        usize::try_from(count).unwrap()
    }
}
