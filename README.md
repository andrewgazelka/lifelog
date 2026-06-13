# lifelog

Continuous local activity log for macOS. One daemon, one SQLite database,
plain SQL as the query interface.

`lifelog record` runs three loops:

| loop | source | table |
|---|---|---|
| sampler (every 5s) | frontmost app via NSWorkspace, window title via CGWindowList, idle time via CGEventSource, lock state via CGSession | `samples` + coalesced `spans` |
| Screen Time ingest (every 15 min) | `~/Library/Application Support/Knowledge/knowledgeC.db` (`/app/usage`, `/app/webUsage`) | `screentime_usage` |
| ingest API (`127.0.0.1:5599`) | anything that can POST JSON, e.g. iOS Shortcuts automations from your phone | `phone_events` |

## Querying

```sh
sqlite3 "$(lifelog db-path)"
```

```sql
-- active seconds per app per day (sampler truth, includes window titles)
SELECT * FROM app_day_seconds WHERE day = date('now', 'localtime') ORDER BY seconds DESC;

-- Screen Time per device per day (includes synced devices when present)
SELECT * FROM screentime_day_seconds ORDER BY day DESC, seconds DESC LIMIT 30;

-- what was I doing at 14:32?
SELECT datetime(start_ms/1000, 'unixepoch', 'localtime'), state, app_name, window_title
FROM spans WHERE start_ms <= 1700000000000 AND end_ms >= 1700000000000;
```

Or the canned report: `lifelog top --days 7`.

## Phone

Apple exposes no public Screen Time API, so phone data arrives two ways:

1. **Screen Time sync**: if "Share Across Devices" is enabled, synced usage
   that lands in knowledgeC.db is ingested automatically with its `device_id`.
2. **Ingest API**: iOS Shortcuts personal automations ("When *app* is
   opened", "When iPhone is unlocked", charger/focus events, ...) can POST:

   ```
   POST http://<mac>:5599/events
   Authorization: Bearer $LIFELOG_TOKEN
   {"kind": "app_opened", "device": "iphone", "payload": {"app": "Instagram"}}
   ```

   Set `LIFELOG_TOKEN` in the daemon's environment to require the bearer
   token; do that before listening beyond localhost (`--listen 0.0.0.0:5599`,
   ideally reachable only over Tailscale).

## Permissions

- **Full Disk Access** (required for Screen Time ingest only): System
  Settings → Privacy & Security → Full Disk Access → add the `lifelog`
  binary. Without it the sampler still records; the ingest loop logs the
  failure and retries. The grant is per-binary path, so re-grant after the
  nix store path changes.
- **Screen Recording** (optional): unlocks window titles. Without it,
  `window_title` is NULL and everything else works.

## Nix

```sh
nix run github:andrewgazelka/lifelog -- record
```

The flake exposes `packages.<system>.lifelog`. Service wiring (launchd via
home-manager) lives in indexable-inc/index `users/andrewgazelka`.
