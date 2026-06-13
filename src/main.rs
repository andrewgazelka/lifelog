//! lifelog: continuous local activity log for macOS.
//!
//! `lifelog record` runs three loops against one SQLite db:
//!   * frontmost-app sampler (every few seconds) -> `samples` + coalesced `spans`
//!   * Screen Time ingest from knowledgeC.db (every few minutes) -> `screentime_usage`
//!   * HTTP ingest API for phone/other-device events -> `phone_events`
//!
//! Everything is plain SQLite, so `sqlite3 "$(lifelog db-path)"` is the query
//! interface; `lifelog top` is just a convenience report.

mod db;
mod mac;
mod sampler;
mod screentime;
mod server;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "lifelog", version, about)]
struct Cli {
    /// SQLite db path (default: ~/Library/Application Support/lifelog/lifelog.db)
    #[arg(long, global = true)]
    db: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the recording daemon (sampler + Screen Time ingest + ingest API).
    Record {
        /// Seconds between frontmost-app samples.
        #[arg(long, default_value_t = 5)]
        interval_secs: u64,
        /// Input silence (seconds) after which time counts as idle.
        #[arg(long, default_value_t = 120)]
        idle_secs: u64,
        /// Seconds between Screen Time (knowledgeC.db) ingest runs.
        #[arg(long, default_value_t = 900)]
        screentime_interval_secs: u64,
        /// Address for the phone-event ingest API. Set LIFELOG_TOKEN to
        /// require a bearer token (do this before binding beyond localhost).
        #[arg(long, default_value = "127.0.0.1:5599")]
        listen: String,
        /// Disable the ingest API.
        #[arg(long)]
        no_listen: bool,
    },
    /// One-shot Screen Time ingest from knowledgeC.db.
    IngestScreentime,
    /// Top apps by active time over the last N days.
    Top {
        #[arg(long, default_value_t = 1)]
        days: i64,
        #[arg(long, default_value_t = 15)]
        limit: i64,
    },
    /// Print the db path.
    DbPath,
}

fn main() -> Result<()> {
    // Die quietly when piped into `head` instead of panicking on EPIPE.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    let cli = Cli::parse();
    let db_path = cli.db.unwrap_or_else(db::default_path);

    match cli.command {
        Command::Record {
            interval_secs,
            idle_secs,
            screentime_interval_secs,
            listen,
            no_listen,
        } => {
            // Create the db (schema + WAL switch) once, before any worker
            // thread opens its own connection: concurrent first-time setup
            // races SQLITE_BUSY even with a busy timeout.
            let conn = db::open(&db_path)?;

            // Screen Time ingest thread: immediate first run, then periodic.
            // Failures (most likely missing Full Disk Access) are logged and
            // retried; the sampler must keep recording regardless.
            let st_db = db_path.clone();
            std::thread::spawn(move || {
                let conn = match db::open(&st_db) {
                    Ok(c) => c,
                    Err(e) => return eprintln!("lifelog: screentime db open failed: {e:#}"),
                };
                loop {
                    match screentime::ingest(&conn, &screentime::knowledge_db_path()) {
                        Ok(n) if n > 0 => eprintln!("lifelog: screentime ingested {n} rows"),
                        Ok(_) => {}
                        Err(e) => eprintln!("lifelog: screentime ingest failed: {e:#}"),
                    }
                    std::thread::sleep(Duration::from_secs(screentime_interval_secs));
                }
            });

            if !no_listen {
                let api_db = db_path.clone();
                std::thread::spawn(move || {
                    if let Err(e) = server::run(&api_db, &listen) {
                        eprintln!("lifelog: ingest API failed: {e:#}");
                    }
                });
            }

            // Sampler owns the main thread (AppKit is friendliest there).
            sampler::run(
                &conn,
                &sampler::Config {
                    interval: Duration::from_secs(interval_secs),
                    idle_threshold: Duration::from_secs(idle_secs),
                },
            )
        }
        Command::IngestScreentime => {
            let conn = db::open(&db_path)?;
            let n = screentime::ingest(&conn, &screentime::knowledge_db_path())?;
            println!("ingested {n} new screentime rows");
            Ok(())
        }
        Command::Top { days, limit } => top(&db_path, days, limit),
        Command::DbPath => {
            println!("{}", db_path.display());
            Ok(())
        }
    }
}

fn top(db_path: &std::path::Path, days: i64, limit: i64) -> Result<()> {
    let conn = db::open(db_path)?;
    let since_ms = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as i64)
        - days * 86_400_000;

    println!("== active spans (sampler), last {days}d ==");
    let mut stmt = conn.prepare(
        "SELECT COALESCE(app_name, bundle_id, '?'), SUM(end_ms - start_ms) / 1000 AS secs
         FROM spans WHERE state = 'active' AND end_ms >= ?1
         GROUP BY 1 ORDER BY secs DESC LIMIT ?2",
    )?;
    let mut rows = stmt.query(rusqlite::params![since_ms, limit])?;
    while let Some(row) = rows.next()? {
        let (name, secs): (String, i64) = (row.get(0)?, row.get(1)?);
        println!("{:>7}  {}", fmt_hms(secs), name);
    }

    println!("\n== screen time (knowledgeC), last {days}d ==");
    let mut stmt = conn.prepare(
        "SELECT COALESCE(device_id, 'this-mac'), value, SUM(end_ms - start_ms) / 1000 AS secs
         FROM screentime_usage WHERE stream = '/app/usage' AND end_ms >= ?1
         GROUP BY 1, 2 ORDER BY secs DESC LIMIT ?2",
    )?;
    let mut rows = stmt.query(rusqlite::params![since_ms, limit])?;
    while let Some(row) = rows.next()? {
        let (device, value, secs): (String, String, i64) = (row.get(0)?, row.get(1)?, row.get(2)?);
        println!("{:>7}  {:10}  {}", fmt_hms(secs), device, value);
    }
    Ok(())
}

fn fmt_hms(secs: i64) -> String {
    format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
}
