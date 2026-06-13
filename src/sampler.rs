use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use rusqlite::{params, Connection};

use crate::mac;

pub struct Config {
    pub interval: Duration,
    /// Input silence after which the span flips from 'active' to 'idle'.
    pub idle_threshold: Duration,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_millis() as i64
}

#[derive(PartialEq, Clone)]
struct SpanKey {
    state: &'static str,
    bundle_id: Option<String>,
    app_name: Option<String>,
    window_title: Option<String>,
}

struct OpenSpan {
    id: i64,
    key: SpanKey,
    last_ms: i64,
}

/// Blocking sampler loop: one `samples` row per tick, plus live-coalesced
/// `spans`. Runs forever; launchd owns restarts.
pub fn run(conn: &Connection, config: &Config) -> Result<()> {
    let mut open: Option<OpenSpan> = None;
    // A gap longer than this (sleep, crash) closes the span at its last
    // observed end instead of stretching it across the gap.
    let max_gap_ms = 3 * config.interval.as_millis() as i64;

    loop {
        let ts = now_ms();
        let idle_ms = mac::idle_ms() as i64;
        let locked = mac::screen_locked();
        let app = mac::frontmost_app();
        let (bundle_id, app_name, pid) = match &app {
            Some(a) => (a.bundle_id.clone(), a.name.clone(), Some(a.pid)),
            None => (None, None, None),
        };
        let window_title = pid.and_then(mac::frontmost_window_title);

        conn.execute(
            "INSERT INTO samples (ts_ms, bundle_id, app_name, pid, window_title, idle_ms, locked)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![ts, bundle_id, app_name, pid, window_title, idle_ms, locked],
        )?;

        let state = if locked {
            "locked"
        } else if idle_ms >= config.idle_threshold.as_millis() as i64 {
            "idle"
        } else {
            "active"
        };
        let key = SpanKey {
            state,
            // Idle/locked spans are app-agnostic so a lunch break is one span,
            // not a series keyed by whatever window was left frontmost.
            bundle_id: (state == "active").then(|| bundle_id.clone()).flatten(),
            app_name: (state == "active").then(|| app_name.clone()).flatten(),
            window_title: (state == "active").then(|| window_title.clone()).flatten(),
        };

        open = Some(advance_span(conn, open, key, ts, max_gap_ms)?);
        std::thread::sleep(config.interval);
    }
}

fn advance_span(
    conn: &Connection,
    open: Option<OpenSpan>,
    key: SpanKey,
    ts: i64,
    max_gap_ms: i64,
) -> Result<OpenSpan> {
    if let Some(span) = open {
        if span.key == key && ts - span.last_ms <= max_gap_ms {
            conn.execute(
                "UPDATE spans SET end_ms = ?1 WHERE id = ?2",
                params![ts, span.id],
            )?;
            return Ok(OpenSpan { last_ms: ts, ..span });
        }
    }
    conn.execute(
        "INSERT INTO spans (start_ms, end_ms, state, bundle_id, app_name, window_title)
         VALUES (?1, ?1, ?2, ?3, ?4, ?5)",
        params![ts, key.state, key.bundle_id, key.app_name, key.window_title],
    )?;
    Ok(OpenSpan {
        id: conn.last_insert_rowid(),
        key,
        last_ms: ts,
    })
}
