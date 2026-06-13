use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OpenFlags};

/// Core Data timestamps count seconds from 2001-01-01 UTC.
const CORE_DATA_EPOCH_UNIX: f64 = 978_307_200.0;

/// Streams worth keeping: app usage (bundle id) and Safari web usage (domain).
const STREAMS: &[&str] = &["/app/usage", "/app/webUsage"];

/// How far back to re-scan on every run. Generous so rows that knowledged
/// flushes late (or that sync in from another device) are still picked up;
/// the uuid primary key makes the overlap idempotent.
const LOOKBACK_SECS: i64 = 14 * 86_400;

pub fn knowledge_db_path() -> PathBuf {
    dirs::home_dir()
        .expect("home dir")
        .join("Library/Application Support/Knowledge/knowledgeC.db")
}

/// Ingest Screen Time events from knowledgeC.db into `screentime_usage`.
/// Returns the number of newly inserted rows.
///
/// knowledged keeps the db open and locked, so we snapshot the db (plus WAL)
/// to a temp dir and read the copy. The copy is also where missing Full Disk
/// Access shows up, as EPERM, so the error message names the fix.
pub fn ingest(conn: &Connection, knowledge_db: &Path) -> Result<u64> {
    let tmp = tempfile_dir()?;
    let snapshot = tmp.join("knowledgeC.db");
    for suffix in ["", "-wal", "-shm"] {
        let src = append_suffix(knowledge_db, suffix);
        if src.exists() {
            std::fs::copy(&src, append_suffix(&snapshot, suffix)).with_context(|| {
                format!(
                    "copying {} (launchd agents need Full Disk Access to read Screen Time data: \
                     System Settings > Privacy & Security > Full Disk Access > add the lifelog binary)",
                    src.display()
                )
            })?;
        } else if suffix.is_empty() {
            bail!("{} does not exist", src.display());
        }
    }

    let kc = Connection::open_with_flags(&snapshot, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .context("opening knowledgeC snapshot")?;

    // Only re-scan the recent window. ZSTARTDATE is Core Data seconds.
    let since_unix_s: i64 = conn.query_row(
        "SELECT COALESCE(MAX(start_ms) / 1000, 0) FROM screentime_usage",
        [],
        |row| row.get(0),
    )?;
    let since_core_data = (since_unix_s - LOOKBACK_SECS) as f64 - CORE_DATA_EPOCH_UNIX;

    let placeholders = STREAMS.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let mut stmt = kc.prepare(&format!(
        "SELECT o.ZUUID, o.ZSTREAMNAME, o.ZVALUESTRING, o.ZSTARTDATE, o.ZENDDATE, s.ZDEVICEID
         FROM ZOBJECT o
         LEFT JOIN ZSOURCE s ON o.ZSOURCE = s.Z_PK
         WHERE o.ZSTREAMNAME IN ({placeholders})
           AND o.ZVALUESTRING IS NOT NULL
           AND o.ZSTARTDATE >= ?"
    ))?;
    let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = STREAMS
        .iter()
        .map(|s| Box::new(*s) as Box<dyn rusqlite::types::ToSql>)
        .collect();
    args.push(Box::new(since_core_data));

    let mut rows = stmt.query(rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())))?;
    let mut insert = conn.prepare(
        "INSERT OR IGNORE INTO screentime_usage (uuid, stream, value, start_ms, end_ms, device_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    let mut inserted = 0;
    while let Some(row) = rows.next()? {
        let uuid: String = row.get(0)?;
        let stream: String = row.get(1)?;
        let value: String = row.get(2)?;
        let start: f64 = row.get(3)?;
        let end: f64 = row.get(4)?;
        let device_id: Option<String> = row.get(5)?;
        let start_ms = ((start + CORE_DATA_EPOCH_UNIX) * 1000.0) as i64;
        let end_ms = ((end + CORE_DATA_EPOCH_UNIX) * 1000.0) as i64;
        inserted += insert.execute(params![uuid, stream, value, start_ms, end_ms, device_id])? as u64;
    }
    Ok(inserted)
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

fn tempfile_dir() -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("lifelog-kc-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}
