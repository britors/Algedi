//! SQLite-backed snapshot of sync state: folder pairs, per-file hashes, and
//! conflicts. Never stores secrets — tokens live in the Secret Service
//! (see PROMPT-ALGEDI.md sec. 4).

use crate::{AccountId, FolderPair, PairId, SyncStatus};
use rusqlite::{params, Connection};
use std::path::Path;

pub struct StateDb {
    conn: Connection,
}

impl StateDb {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    pub fn open_in_memory() -> rusqlite::Result<Self> {
        let conn = Connection::open_in_memory()?;
        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    fn init_schema(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS folder_pairs (
                id                TEXT PRIMARY KEY,
                account_id        TEXT NOT NULL,
                local_path        TEXT NOT NULL,
                remote_path       TEXT NOT NULL,
                remote_folder_id  TEXT NOT NULL,
                change_cursor     TEXT,
                paused            INTEGER NOT NULL DEFAULT 0
            );

            -- local_hash/remote_hash are kept separate because each
            -- provider uses its own content-hash algorithm (Drive: md5,
            -- OneDrive: quickXorHash) that we never compute ourselves; the
            -- diff in sync_engine.rs compares each side against its own
            -- last-known value, not against each other.
            CREATE TABLE IF NOT EXISTS files (
                pair_id         TEXT NOT NULL REFERENCES folder_pairs(id) ON DELETE CASCADE,
                relative_path   TEXT NOT NULL,
                remote_id       TEXT,
                local_hash      TEXT,
                remote_hash     TEXT,
                status          TEXT NOT NULL DEFAULT 'unknown',
                PRIMARY KEY (pair_id, relative_path)
            );

            CREATE TABLE IF NOT EXISTS conflicts (
                id                      TEXT PRIMARY KEY,
                pair_id                 TEXT NOT NULL REFERENCES folder_pairs(id) ON DELETE CASCADE,
                relative_path           TEXT NOT NULL,
                conflicting_copy_path   TEXT NOT NULL,
                detected_at             TEXT NOT NULL
            );
            "#,
        )
    }

    pub fn insert_folder_pair(&self, pair: &FolderPair) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO folder_pairs (id, account_id, local_path, remote_path, remote_folder_id, paused)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                pair.id.to_string(),
                pair.account_id.to_string(),
                pair.local_path.to_string_lossy(),
                pair.remote_path,
                pair.remote_folder_id,
                pair.paused as i64,
            ],
        )?;
        Ok(())
    }

    pub fn remove_folder_pair(&self, pair_id: PairId) -> rusqlite::Result<()> {
        self.conn.execute(
            "DELETE FROM folder_pairs WHERE id = ?1",
            params![pair_id.to_string()],
        )?;
        Ok(())
    }

    pub fn set_paused(&self, pair_id: PairId, paused: bool) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE folder_pairs SET paused = ?2 WHERE id = ?1",
            params![pair_id.to_string(), paused as i64],
        )?;
        Ok(())
    }

    pub fn get_change_cursor(&self, pair_id: PairId) -> rusqlite::Result<Option<String>> {
        self.conn.query_row(
            "SELECT change_cursor FROM folder_pairs WHERE id = ?1",
            params![pair_id.to_string()],
            |row| row.get(0),
        )
    }

    pub fn set_change_cursor(&self, pair_id: PairId, cursor: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE folder_pairs SET change_cursor = ?2 WHERE id = ?1",
            params![pair_id.to_string(), cursor],
        )?;
        Ok(())
    }

    pub fn file_status(&self, pair_id: PairId, relative_path: &str) -> rusqlite::Result<SyncStatus> {
        let status: Option<String> = self
            .conn
            .query_row(
                "SELECT status FROM files WHERE pair_id = ?1 AND relative_path = ?2",
                params![pair_id.to_string(), relative_path],
                |row| row.get(0),
            )
            .ok();

        Ok(match status.as_deref() {
            Some("synced") => SyncStatus::Synced,
            Some("syncing") => SyncStatus::Syncing,
            Some("conflict") => SyncStatus::Conflict,
            Some("paused") => SyncStatus::Paused,
            _ => SyncStatus::Unknown,
        })
    }

    /// Last-known local/remote content hashes recorded for this file, or
    /// `(None, None)` if it has never been synced before.
    pub fn get_hashes(
        &self,
        pair_id: PairId,
        relative_path: &str,
    ) -> rusqlite::Result<(Option<String>, Option<String>)> {
        let result = self.conn.query_row(
            "SELECT local_hash, remote_hash FROM files WHERE pair_id = ?1 AND relative_path = ?2",
            params![pair_id.to_string(), relative_path],
            |row| Ok((row.get(0)?, row.get(1)?)),
        );
        match result {
            Ok(hashes) => Ok(hashes),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok((None, None)),
            Err(e) => Err(e),
        }
    }

    /// Records the given hashes for `relative_path` under `status` (one of
    /// the `SyncStatus::as_str()` values). Used directly when the status
    /// isn't plain `"synced"` — e.g. `apply_conflict` marks the
    /// auto-resolved original path as `"conflict"` so it keeps flagging
    /// for the user's attention until reviewed.
    pub fn record_file_state(
        &self,
        pair_id: PairId,
        relative_path: &str,
        remote_id: Option<&str>,
        local_hash: Option<&str>,
        remote_hash: Option<&str>,
        status: &str,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO files (pair_id, relative_path, remote_id, local_hash, remote_hash, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(pair_id, relative_path) DO UPDATE SET
                remote_id = excluded.remote_id,
                local_hash = excluded.local_hash,
                remote_hash = excluded.remote_hash,
                status = excluded.status",
            params![pair_id.to_string(), relative_path, remote_id, local_hash, remote_hash, status],
        )?;
        Ok(())
    }

    /// Records that `relative_path` is now synced at the given hashes,
    /// marking it `status = 'synced'`.
    pub fn record_synced(
        &self,
        pair_id: PairId,
        relative_path: &str,
        remote_id: Option<&str>,
        local_hash: Option<&str>,
        remote_hash: Option<&str>,
    ) -> rusqlite::Result<()> {
        self.record_file_state(pair_id, relative_path, remote_id, local_hash, remote_hash, "synced")
    }

    pub fn remove_file_record(&self, pair_id: PairId, relative_path: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "DELETE FROM files WHERE pair_id = ?1 AND relative_path = ?2",
            params![pair_id.to_string(), relative_path],
        )?;
        Ok(())
    }

    /// Records that both versions of a file were kept: `relative_path` at
    /// its original location (now holding the synced/remote version) and
    /// `conflicting_copy_path` holding what used to be there locally. See
    /// PROMPT-ALGEDI.md sec. 5.3.
    pub fn record_conflict(
        &self,
        pair_id: PairId,
        relative_path: &str,
        conflicting_copy_path: &Path,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO conflicts (id, pair_id, relative_path, conflicting_copy_path, detected_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                uuid::Uuid::new_v4().to_string(),
                pair_id.to_string(),
                relative_path,
                conflicting_copy_path.to_string_lossy(),
                chrono::Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// All recorded conflicts, most recent first: (pair_id, relative_path,
    /// conflicting_copy_path, detected_at).
    pub fn list_conflicts(&self) -> rusqlite::Result<Vec<(PairId, String, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT pair_id, relative_path, conflicting_copy_path, detected_at
             FROM conflicts ORDER BY detected_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (pair_id_str, relative_path, conflicting_copy_path, detected_at) = r?;
            if let Ok(pair_id) = uuid::Uuid::parse_str(&pair_id_str) {
                out.push((pair_id, relative_path, conflicting_copy_path, detected_at));
            }
        }
        Ok(out)
    }

    pub fn accounts_referenced(&self) -> rusqlite::Result<Vec<AccountId>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT account_id FROM folder_pairs")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            if let Ok(id) = uuid::Uuid::parse_str(&r?) {
                out.push(id);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_change_cursor() {
        let db = StateDb::open_in_memory().unwrap();
        let pair = FolderPair {
            id: uuid::Uuid::new_v4(),
            account_id: uuid::Uuid::new_v4(),
            local_path: "/home/user/Documents".into(),
            remote_path: "/Documents".into(),
            remote_folder_id: "abc123".into(),
            paused: false,
        };
        db.insert_folder_pair(&pair).unwrap();
        assert_eq!(db.get_change_cursor(pair.id).unwrap(), None);

        db.set_change_cursor(pair.id, "token-1").unwrap();
        assert_eq!(db.get_change_cursor(pair.id).unwrap(), Some("token-1".into()));
    }
}
