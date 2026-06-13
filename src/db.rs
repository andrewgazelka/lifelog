use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::Connection;

/// Default db location: `~/Library/Application Support/lifelog/lifelog.db` on
/// macOS (via `dirs::data_dir`), so the db sits next to the other app data the
/// OS already backs up.
pub fn default_path() -> PathBuf {
    dirs::data_dir()
        .expect("platform data dir")
        .join("lifelog")
        .join("lifelog.db")
}

pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let conn = Connection::open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    // Timeout first: the daemon's three threads each open a connection at
    // startup, and without it the losers of the journal-mode/schema race fail
    // with SQLITE_BUSY instead of waiting.
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    // WAL so the sampler, the ingester, the API server, and ad-hoc `sqlite3`
    // readers never block each other.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

const SCHEMA: &str = r#"
-- Raw sampler ticks (one row per --interval-secs while recording).
CREATE TABLE IF NOT EXISTS samples (
    ts_ms        INTEGER NOT NULL,  -- unix epoch ms
    bundle_id    TEXT,
    app_name     TEXT,
    pid          INTEGER,
    window_title TEXT,              -- NULL unless Screen Recording permission is granted
    idle_ms      INTEGER NOT NULL,
    locked       INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_samples_ts ON samples (ts_ms);

-- Sampler ticks coalesced into contiguous state spans, maintained live by the
-- daemon so usage queries are a plain SUM instead of an islands problem.
CREATE TABLE IF NOT EXISTS spans (
    id           INTEGER PRIMARY KEY,
    start_ms     INTEGER NOT NULL,
    end_ms       INTEGER NOT NULL,
    state        TEXT NOT NULL,     -- 'active' | 'idle' | 'locked'
    bundle_id    TEXT,
    app_name     TEXT,
    window_title TEXT
);
CREATE INDEX IF NOT EXISTS idx_spans_start ON spans (start_ms);

-- macOS Screen Time events ingested from knowledgeC.db. device_id is NULL for
-- this Mac; synced devices (iPhone etc., when Screen Time shares across
-- devices) carry their knowledgeC ZSOURCE.ZDEVICEID.
CREATE TABLE IF NOT EXISTS screentime_usage (
    uuid      TEXT PRIMARY KEY,     -- knowledgeC ZOBJECT.ZUUID, makes ingest idempotent
    stream    TEXT NOT NULL,        -- '/app/usage' | '/app/webUsage'
    value     TEXT NOT NULL,        -- bundle id (or domain for webUsage)
    start_ms  INTEGER NOT NULL,
    end_ms    INTEGER NOT NULL,
    device_id TEXT
);
CREATE INDEX IF NOT EXISTS idx_screentime_start ON screentime_usage (start_ms);

-- Events pushed over the HTTP API (e.g. iOS Shortcuts automations).
CREATE TABLE IF NOT EXISTS phone_events (
    id      INTEGER PRIMARY KEY,
    ts_ms   INTEGER NOT NULL,
    device  TEXT,
    kind    TEXT NOT NULL,
    payload TEXT                    -- raw JSON
);
CREATE INDEX IF NOT EXISTS idx_phone_events_ts ON phone_events (ts_ms);

-- Canned per-day rollups for ad-hoc `sqlite3` querying.
CREATE VIEW IF NOT EXISTS app_day_seconds AS
SELECT date(start_ms / 1000, 'unixepoch', 'localtime') AS day,
       bundle_id,
       app_name,
       SUM(end_ms - start_ms) / 1000.0 AS seconds
FROM spans
WHERE state = 'active'
GROUP BY day, bundle_id, app_name;

CREATE VIEW IF NOT EXISTS screentime_day_seconds AS
SELECT date(start_ms / 1000, 'unixepoch', 'localtime') AS day,
       COALESCE(device_id, 'this-mac') AS device,
       value,
       SUM(end_ms - start_ms) / 1000.0 AS seconds
FROM screentime_usage
WHERE stream = '/app/usage'
GROUP BY day, device, value;
"#;
