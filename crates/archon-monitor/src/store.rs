//! Structured local store for build events, so a report can be regenerated
//! after the fact (`archon install --report`) without re-running the build.

use std::path::Path;

use archon_monitor_common::{EventKind, SyscallEvent};
use rusqlite::{Connection, params};

use crate::policy::FlagReason;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("event store error: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

pub struct EventStore {
    conn: Connection,
}

/// One event as read back from the store, with the recorded verdict.
pub struct StoredEvent {
    pub event: SyscallEvent,
    pub flag_reason: Option<String>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS builds (
    id          INTEGER PRIMARY KEY,
    package     TEXT NOT NULL,
    started_at  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS events (
    id          INTEGER PRIMARY KEY,
    build_id    INTEGER NOT NULL REFERENCES builds(id),
    kind        INTEGER NOT NULL,
    pid         INTEGER NOT NULL,
    timestamp_ns INTEGER NOT NULL,
    comm        TEXT NOT NULL,
    path        TEXT NOT NULL,
    open_flags  INTEGER NOT NULL,
    addr_v4     INTEGER NOT NULL,
    port        INTEGER NOT NULL,
    flag_reason TEXT
);
CREATE INDEX IF NOT EXISTS events_build_id ON events(build_id);
";

impl EventStore {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// Registers a new build, returning its id for [`EventStore::record_event`].
    pub fn start_build(&self, package: &str, started_at_unix: i64) -> Result<i64, StoreError> {
        self.conn.execute(
            "INSERT INTO builds (package, started_at) VALUES (?1, ?2)",
            params![package, started_at_unix],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn record_event(
        &self,
        build_id: i64,
        event: &SyscallEvent,
        flag_reason: Option<&FlagReason>,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO events \
             (build_id, kind, pid, timestamp_ns, comm, path, open_flags, addr_v4, port, flag_reason) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                build_id,
                event.kind,
                event.pid,
                event.timestamp_ns as i64,
                event.comm_str(),
                event.path_str(),
                event.open_flags,
                event.addr_v4,
                event.port,
                flag_reason.map(|r| r.to_string()),
            ],
        )?;
        Ok(())
    }

    pub fn events_for_build(&self, build_id: i64) -> Result<Vec<StoredEvent>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT kind, pid, timestamp_ns, comm, path, open_flags, addr_v4, port, flag_reason \
             FROM events WHERE build_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![build_id], |row| {
            let mut event = SyscallEvent::zeroed(
                EventKind::from_u8(row.get::<_, u8>(0)?).unwrap_or(EventKind::Open),
            );
            event.pid = row.get(1)?;
            event.timestamp_ns = row.get::<_, i64>(2)? as u64;
            let comm: String = row.get(3)?;
            let comm_bytes = comm.as_bytes();
            let n = comm_bytes.len().min(event.comm.len());
            event.comm[..n].copy_from_slice(&comm_bytes[..n]);
            let path: String = row.get(4)?;
            let path_bytes = path.as_bytes();
            let n = path_bytes.len().min(event.path.len());
            event.path[..n].copy_from_slice(&path_bytes[..n]);
            event.path_len = n as u16;
            event.open_flags = row.get(5)?;
            event.addr_v4 = row.get(6)?;
            event.port = row.get(7)?;
            let flag_reason: Option<String> = row.get(8)?;
            Ok(StoredEvent { event, flag_reason })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use archon_monitor_common::EventKind;

    fn sample_event(path: &str) -> SyscallEvent {
        let mut ev = SyscallEvent::zeroed(EventKind::Open);
        ev.pid = 1234;
        ev.timestamp_ns = 999;
        ev.path[..path.len()].copy_from_slice(path.as_bytes());
        ev.path_len = path.len() as u16;
        ev
    }

    #[test]
    fn records_and_reads_back_events() {
        let store = EventStore::open_in_memory().unwrap();
        let build_id = store.start_build("tty-clock", 1_700_000_000).unwrap();

        store
            .record_event(build_id, &sample_event("/usr/include/stdio.h"), None)
            .unwrap();
        let flagged = sample_event("/etc/passwd");
        store
            .record_event(
                build_id,
                &flagged,
                Some(&FlagReason::WriteOutsideBuildDir {
                    path: "/etc/passwd".into(),
                }),
            )
            .unwrap();

        let events = store.events_for_build(build_id).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event.path_str(), "/usr/include/stdio.h");
        assert!(events[0].flag_reason.is_none());
        assert_eq!(events[1].event.path_str(), "/etc/passwd");
        assert!(
            events[1]
                .flag_reason
                .as_deref()
                .unwrap()
                .contains("/etc/passwd")
        );
    }

    #[test]
    fn separate_builds_dont_mix_events() {
        let store = EventStore::open_in_memory().unwrap();
        let build_a = store.start_build("pkg-a", 1).unwrap();
        let build_b = store.start_build("pkg-b", 2).unwrap();
        store
            .record_event(build_a, &sample_event("/a"), None)
            .unwrap();
        store
            .record_event(build_b, &sample_event("/b"), None)
            .unwrap();

        assert_eq!(store.events_for_build(build_a).unwrap().len(), 1);
        assert_eq!(store.events_for_build(build_b).unwrap().len(), 1);
    }
}
