//! Ordered, embedded schema migrations.
//!
//! Each migration runs in its own `BEGIN IMMEDIATE` transaction together with the row that records
//! it in `schema_migrations`, so a crash mid-migration leaves the previous version intact. Before
//! any migration touches a database that already holds objects, the whole database is copied with
//! SQLite's online backup API; if that copy fails, nothing is migrated.

use crate::{Result, StorageError};
use rsrewind_core::{DataDir, Timestamp};
use rusqlite::{Connection, Transaction, TransactionBehavior};
use std::path::PathBuf;

/// One schema step. `version`s must start at 1 and increase by exactly 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Migration {
    pub version: u32,
    pub name: &'static str,
    pub sql: &'static str,
}

/// Every migration this build knows, in order. Released entries are immutable.
pub const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "initial",
    sql: include_str!("sql/0001_initial.sql"),
}];

/// The schema version this build writes and expects.
pub const SCHEMA_VERSION: u32 = 1;

const _: () = assert!(MIGRATIONS[MIGRATIONS.len() - 1].version == SCHEMA_VERSION);

/// What `migrate` did, for logging and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationOutcome {
    pub from: u32,
    pub to: u32,
    pub backup: Option<PathBuf>,
}

/// Highest applied version, or 0 for a database that has never been migrated.
pub(crate) fn current_version(conn: &Connection) -> Result<u32> {
    let has_table: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' \
         AND name = 'schema_migrations')",
        [],
        |row| row.get(0),
    )?;
    if !has_table {
        return Ok(0);
    }
    let version: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    u32::try_from(version)
        .map_err(|_| StorageError::Corrupt(format!("schema_migrations version {version}")))
}

fn validate(migrations: &[Migration]) -> Result<()> {
    for (index, migration) in migrations.iter().enumerate() {
        let expected = u32::try_from(index + 1).unwrap_or(u32::MAX);
        if migration.version != expected {
            return Err(StorageError::InvalidArgument(format!(
                "migration list out of order: position {index} has version {}",
                migration.version
            )));
        }
    }
    Ok(())
}

fn has_user_objects(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' \
         AND name <> 'schema_migrations')",
        [],
        |row| row.get(0),
    )?)
}

/// Brings `conn` up to the last entry of `migrations`.
pub(crate) fn migrate(
    conn: &Connection,
    data: &DataDir,
    migrations: &[Migration],
) -> Result<MigrationOutcome> {
    validate(migrations)?;
    let latest = migrations.last().map_or(0, |m| m.version);
    let from = current_version(conn)?;
    if from > latest {
        return Err(StorageError::SchemaTooNew {
            found: from,
            supported: latest,
        });
    }
    let pending: Vec<&Migration> = migrations.iter().filter(|m| m.version > from).collect();
    if pending.is_empty() {
        return Ok(MigrationOutcome {
            from,
            to: from,
            backup: None,
        });
    }

    let backup = if has_user_objects(conn)? {
        Some(backup_database(conn, data, from)?)
    } else {
        None
    };

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version    INTEGER PRIMARY KEY,
             name       TEXT NOT NULL,
             applied_at INTEGER NOT NULL
         )",
    )?;

    for migration in pending {
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        // Another process (the daemon opens two stores) may have applied it while we waited for
        // the write lock.
        let already: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM schema_migrations WHERE version = ?1)",
            [migration.version],
            |row| row.get(0),
        )?;
        if already {
            continue;
        }
        tx.execute_batch(migration.sql)
            .map_err(|source| StorageError::Migration {
                version: migration.version,
                name: migration.name,
                source,
            })?;
        tx.execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![migration.version, migration.name, Timestamp::now().0],
        )?;
        tx.commit()?;
        tracing::info!(
            version = migration.version,
            name = migration.name,
            "applied schema migration"
        );
    }

    Ok(MigrationOutcome {
        from,
        to: latest,
        backup,
    })
}

/// Copies the live database to `backups/recall-v{from}-{utc}.db` with the online backup API.
fn backup_database(conn: &Connection, data: &DataDir, from: u32) -> Result<PathBuf> {
    let dir = data.backups();
    std::fs::create_dir_all(&dir)
        .map_err(|e| StorageError::BackupFailed(format!("{}: {e}", dir.display())))?;
    let stamp = Timestamp::now().to_utc().format("%Y%m%dT%H%M%S%3fZ");
    let mut path = dir.join(format!("recall-v{from}-{stamp}.db"));
    let mut attempt = 1;
    // Never write into an existing file: the backup API would overwrite an older backup.
    while path.exists() {
        attempt += 1;
        if attempt > 100 {
            return Err(StorageError::BackupFailed(format!(
                "no free backup file name in {}",
                dir.display()
            )));
        }
        path = dir.join(format!("recall-v{from}-{stamp}-{attempt}.db"));
    }
    if let Err(error) = conn.backup(rusqlite::MAIN_DB, &path, None) {
        // A partial copy is worse than none: it looks like a backup but is not one.
        let _ = std::fs::remove_file(&path);
        return Err(StorageError::BackupFailed(format!(
            "{}: {error}",
            path.display()
        )));
    }
    tracing::info!(from, path = %path.display(), "backed up database before migrating");
    Ok(path)
}
