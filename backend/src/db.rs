//! SQLite connection pool and embedded migrations.
//!
//! Ported from phos `backend/src/db.rs`, with `foreign_keys=ON` added because
//! picweight's schema leans on `ON DELETE CASCADE` (deleting a meal must take
//! its items, corrections, jobs, steps and session with it) — and SQLite
//! ignores foreign keys unless the pragma is set *per connection*.

use crate::error::AppError;
use diesel::connection::SimpleConnection;
use diesel::prelude::*;
use diesel::r2d2::{self, ConnectionManager, CustomizeConnection, Pool, PooledConnection};
use diesel::sqlite::SqliteConnection;
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
use std::path::Path;
use tracing::info;

/// Migrations compiled into the binary; no `migrations/` directory is shipped.
pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

/// The r2d2 pool type used everywhere.
pub type DbPool = Pool<ConnectionManager<SqliteConnection>>;

/// A checked-out connection.
pub type DbConnection = PooledConnection<ConnectionManager<SqliteConnection>>;

/// Pool size. This is a 2–10 user homelab app whose writes are serialized by
/// SQLite anyway; a handful of connections is plenty and keeps WAL contention
/// (and memory) minimal.
pub const MAX_POOL_SIZE: u32 = 8;

/// How long a statement waits on a write lock before returning `SQLITE_BUSY`.
pub const BUSY_TIMEOUT_MS: u32 = 60_000;

/// Applies the per-connection pragmas SQLite needs to behave under concurrency.
#[derive(Debug)]
struct SqlitePragmaCustomizer;

impl CustomizeConnection<SqliteConnection, r2d2::Error> for SqlitePragmaCustomizer {
    fn on_acquire(&self, conn: &mut SqliteConnection) -> Result<(), r2d2::Error> {
        // Order matters. `busy_timeout` must be set BEFORE `journal_mode`:
        // switching to WAL needs a brief exclusive lock, and without an active
        // timeout a concurrent open gets SQLITE_BUSY immediately instead of
        // waiting. (Same reasoning as phos.)
        //
        // `foreign_keys` is per-connection in SQLite and defaults to OFF, so it
        // has to be re-applied on every acquire or the CASCADE rules in the
        // migration are silently inert.
        conn.batch_execute(&format!(
            "PRAGMA busy_timeout = {BUSY_TIMEOUT_MS};\
             PRAGMA journal_mode = WAL;\
             PRAGMA foreign_keys = ON;\
             PRAGMA synchronous = NORMAL;"
        ))
        .map_err(r2d2::Error::QueryError)
    }
}

/// Build the connection pool for a SQLite database file.
pub fn establish_pool<P: AsRef<Path>>(path: P) -> Result<DbPool, AppError> {
    let database_url = path.as_ref().to_string_lossy().to_string();
    establish_pool_from_url(&database_url)
}

/// Build the connection pool from a diesel database URL (a file path, or
/// `:memory:` in tests).
pub fn establish_pool_from_url(database_url: &str) -> Result<DbPool, AppError> {
    let manager = ConnectionManager::<SqliteConnection>::new(database_url);
    Pool::builder()
        .max_size(MAX_POOL_SIZE)
        .min_idle(Some(1))
        .connection_customizer(Box::new(SqlitePragmaCustomizer))
        .build(manager)
        .map_err(AppError::from)
}

/// Run every pending embedded migration.
pub fn run_migrations(pool: &DbPool) -> Result<(), AppError> {
    let mut conn = pool.get()?;
    let applied = conn
        .run_pending_migrations(MIGRATIONS)
        .map_err(|e| AppError::Internal(format!("migration failed: {e}")))?;
    if applied.is_empty() {
        info!("database schema is up to date");
    } else {
        info!("applied {} migration(s)", applied.len());
    }
    Ok(())
}

/// Open a standalone connection with the same pragmas as the pool.
///
/// For worker threads that want a connection outside the pool's budget.
pub fn open_connection(database_url: &str) -> Result<SqliteConnection, AppError> {
    let mut conn = SqliteConnection::establish(database_url)?;
    conn.batch_execute(&format!(
        "PRAGMA busy_timeout = {BUSY_TIMEOUT_MS};\
         PRAGMA journal_mode = WAL;\
         PRAGMA foreign_keys = ON;\
         PRAGMA synchronous = NORMAL;"
    ))?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_apply_to_a_fresh_database() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("test.db");
        let pool = establish_pool(&db).unwrap();
        run_migrations(&pool).unwrap();
        // Running twice must be a no-op, not an error.
        run_migrations(&pool).unwrap();

        let mut conn = pool.get().unwrap();
        // Foreign keys must be on for this connection.
        #[derive(QueryableByName)]
        struct Flag {
            #[diesel(sql_type = diesel::sql_types::Integer)]
            foreign_keys: i32,
        }
        let flag: Flag = diesel::sql_query("PRAGMA foreign_keys")
            .get_result(&mut conn)
            .unwrap();
        assert_eq!(flag.foreign_keys, 1);
    }
}
