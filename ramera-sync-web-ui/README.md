# ramera-sync-web-ui

Lightweight server-rendered web UI (Axum) for browsing `ramera-sync` records directly from Backblaze B2.

## Features

- No frontend framework.
- Day list view from B2 prefix.
- Day detail view with snapshot/raw/clips grouped by camera.
- Download/open links proxied through this server.
- Request-level logs for each UI operation (`index`, `day`, `download`, B2 auth/list pages, failures).
- Cleaner layout with metrics, search/filter, and per-camera grouping.

## Run

```bash
cargo run
```

Default bind:

- `UI_BIND=0.0.0.0:8080`

Config source order:

1. Env vars (`B2_KEY_ID`, `B2_APPLICATION_KEY`, `B2_BUCKET_ID`, optional `B2_FILE_PREFIX`, `B2_API_BASE`)
2. `settings.conf` path from `RAMERA_SYNC_CONFIG` (default: `settings.conf` in current dir)

Example with backend config file:

```bash
RAMERA_SYNC_CONFIG=../ramera-sync/settings.conf cargo run
```

Then open:

- `http://<host>:8080`

## Logs

The UI prints step logs to stderr/stdout in this format:

```text
[ui][1700000000][op:12] b2.list.page | page=1 files=1000
```

This lets you track exactly what the UI is doing for each request.
