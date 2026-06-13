use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::params;
use serde_json::Value;
use tiny_http::{Header, Method, Response, Server, StatusCode};

use crate::db;

/// Minimal ingest API so other devices can push events, e.g. an iOS Shortcuts
/// automation ("When Instagram is opened") doing a POST:
///
///   POST /events
///   Authorization: Bearer $LIFELOG_TOKEN   (required iff the env var is set)
///   {"kind": "app_opened", "device": "iphone", "payload": {"app": "Instagram"}}
///
/// Body may also be a JSON array of such objects. `ts_ms` defaults to arrival
/// time. Runs forever; launchd owns restarts.
pub fn run(db_path: &Path, listen: &str) -> Result<()> {
    let token = std::env::var("LIFELOG_TOKEN").ok().filter(|t| !t.is_empty());
    let conn = db::open(db_path)?;
    let server = Server::http(listen)
        .map_err(|e| anyhow::anyhow!("binding {listen}: {e}"))
        .context("starting ingest API")?;
    eprintln!("lifelog: ingest API listening on http://{listen}/events");

    for mut request in server.incoming_requests() {
        let response = handle(&conn, &token, &mut request);
        let _ = request.respond(response);
    }
    Ok(())
}

fn handle(
    conn: &rusqlite::Connection,
    token: &Option<String>,
    request: &mut tiny_http::Request,
) -> Response<std::io::Cursor<Vec<u8>>> {
    let json_header = Header::from_bytes("Content-Type", "application/json").expect("static header");
    let reply = |status: u16, body: &str| {
        Response::from_string(body)
            .with_status_code(StatusCode(status))
            .with_header(json_header.clone())
    };

    if request.method() == &Method::Get && request.url() == "/health" {
        return reply(200, r#"{"ok":true}"#);
    }
    if request.method() != &Method::Post || request.url() != "/events" {
        return reply(404, r#"{"error":"POST /events or GET /health"}"#);
    }
    if let Some(expected) = token {
        let authorized = request
            .headers()
            .iter()
            .find(|h| h.field.equiv("Authorization"))
            .map(|h| h.value.as_str() == format!("Bearer {expected}"))
            .unwrap_or(false);
        if !authorized {
            return reply(401, r#"{"error":"bad or missing bearer token"}"#);
        }
    }

    let mut body = String::new();
    if request.as_reader().read_to_string(&mut body).is_err() {
        return reply(400, r#"{"error":"unreadable body"}"#);
    }
    let parsed: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => return reply(400, &format!(r#"{{"error":"invalid json: {e}"}}"#)),
    };
    let events = match parsed {
        Value::Array(items) => items,
        v => vec![v],
    };

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_millis() as i64;
    let mut inserted = 0;
    for event in &events {
        let Some(kind) = event.get("kind").and_then(Value::as_str) else {
            return reply(400, r#"{"error":"each event needs a string 'kind'"}"#);
        };
        let ts_ms = event.get("ts_ms").and_then(Value::as_i64).unwrap_or(now_ms);
        let device = event.get("device").and_then(Value::as_str);
        let payload = event.get("payload").map(Value::to_string);
        let ok = conn.execute(
            "INSERT INTO phone_events (ts_ms, device, kind, payload) VALUES (?1, ?2, ?3, ?4)",
            params![ts_ms, device, kind, payload],
        );
        match ok {
            Ok(n) => inserted += n,
            Err(e) => return reply(500, &format!(r#"{{"error":"{e}"}}"#)),
        }
    }
    reply(200, &format!(r#"{{"ok":true,"inserted":{inserted}}}"#))
}
