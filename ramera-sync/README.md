# ramera-sync

`ramera-sync` is a Rust CLI for NVR discovery, record metadata collection, local clip download, and optional Backblaze B2 upload.

## What it does

- Scans your LAN CIDR to find NVR devices.
- Detects provider module (`hikvision` implemented, `generic` fallback).
- Pulls record metadata over HTTP/ISAPI.
- Downloads actual video clips over RTSP with `ffmpeg`.
- Stores files locally first under `records/`.
- In cloud mode, uploads daily records + clips to Backblaze B2 with retention cleanup.

## Project layout

- Config: `settings.conf` — runtime configuration (may contain sensitive credentials; do not commit to public repositories)
- Local ffmpeg binary: `ffmpeg/ffmpeg`
- Daily snapshot: `records/snapshot-YYYYMMDD.json`
- Daily raw metadata files: `records/raw/YYYYMMDD/`
- Downloaded video clips: `records/clips/<timestamp>/` (manual `video-clips`)
- `run-local` cycle clips: `records/clips/snapshot-<YYYYMMDD>/`

## Setup

1. Create config:

```bash
cargo run -- init-config --path settings.conf
```

2. Edit `settings.conf` (minimum fields):

```conf
scan.cidr=192.168.1.0/24
nvr.username=admin
nvr.password=YOUR_PASSWORD
```

3. Install local ffmpeg into this project folder:

```bash
cargo run -- install-ffmpeg --dir ffmpeg
```

## Command reference

```bash
# Show all commands
cargo run -- --help

# Show video-clips options
cargo run -- video-clips --help

# Run end-to-end smoke test (healthcheck + discover + records + 30s clip + B2 upload verify)
cargo run -- test-mode --config settings.conf

# Validate runtime dependencies/config (and optional B2 connectivity)
cargo run -- healthcheck --config settings.conf
cargo run -- healthcheck --config settings.conf --check-b2
```

```bash
# Discover devices
cargo run -- discover --config settings.conf

# Fetch metadata only (JSON snapshot + raw XML/JSON payloads)
cargo run -- video-records --config settings.conf

# Fetch metadata and print snapshot JSON
cargo run -- video-records --config settings.conf --json

# Download video clips (last 1 day, max 3 clips, each up to 10s)
cargo run -- video-clips --config settings.conf --days 1 --max-clips 3 --clip-seconds 10

# End-to-end smoke test with explicit options (example)
cargo run -- test-mode --config settings.conf --days 1 --max-clips 1 --clip-seconds 30

# Skip B2 verification when testing local-only
cargo run -- test-mode --config settings.conf --no-b2

# Download ALL clips found in range (0 means no clip limit)
cargo run -- video-clips --config settings.conf --days 30 --max-clips 0 --clip-seconds 30

# Download ALL full records directly (no clip limit, no duration trim)
cargo run -- video-clips --config settings.conf --days 30 --max-clips 0 --clip-seconds 0

# Local-only loop (no B2 upload). Each cycle saves clips under:
# records/clips/snapshot-<YYYYMMDD>/
cargo run -- run-local --config settings.conf

# Full sync once (local save + B2 upload)
cargo run -- sync-once --config settings.conf

# Full periodic loop (local + B2 day-based upload/cleanup)
cargo run -- run --config settings.conf
```

## Testing flow (recommended)

1. `cargo run -- test-mode --config settings.conf`
2. If needed, run each step manually for troubleshooting:
   - `cargo run -- discover --config settings.conf`
   - `cargo run -- video-records --config settings.conf`
   - `cargo run -- video-clips --config settings.conf --days 1 --max-clips 1 --clip-seconds 30`
   - `cargo run -- sync-once --config settings.conf` (B2 upload path)
3. Check outputs:
   - `records/snapshot-YYYYMMDD.json`
   - `records/raw/YYYYMMDD/`
   - `records/clips/<timestamp>/` or `records/clips/snapshot-<YYYYMMDD>/`

## Backblaze B2 (optional)

Set these in `settings.conf` when you want uploads:

```conf
b2.key_id=${B2_KEY_ID}
b2.application_key=${B2_APPLICATION_KEY}
b2.bucket_id=${B2_BUCKET_ID}
b2.file_prefix=ramera/nvr-snapshots
b2.max_retentation_days=60
b2.upload_lag_days=1
```

`video-records` and `run-local` do not require B2 credentials.

## Notes

- Config format is plain text `key=value` (not TOML).
- NVR HTTP auth supports Basic and Digest.
- `video-clips` ffmpeg lookup order:
  - `FFMPEG_BIN` env var
  - `ffmpeg/ffmpeg`
  - system `ffmpeg`
- `run` and `run-local` prefer existing runtime `ffmpeg`/`ffprobe` (env var or system). If unavailable, they auto-install local binaries into `ffmpeg/` using `scripts/install_ffmpeg.sh`.
- B2 uploads use SHA1 verification (`sha1sum`, `shasum`, or `openssl`) plus response metadata checks.
- `video-clips` with `--clip-seconds 0` downloads full-length records.
- `run-local` now always downloads all clips for the same snapshot cycle into a single folder:
  - `records/clips/snapshot-<YYYYMMDD>/`
- `run` / `sync-once` cloud behavior:
  - `run` processes one cloud upload cycle per UTC day (not every scheduler tick)
  - upload lag is controlled by `b2.upload_lag_days` (default `1`): only days older than this lag are uploaded/finalized
  - deferred (too recent) days stay local-only until they become eligible for cloud upload
  - if a deferred day has `_complete.json` accidentally, marker is removed so the day can continue updating locally
  - when a day becomes eligible, it uploads `snapshot/raw/clips` and writes `_complete.json`
  - uploads day-by-day under `b2.file_prefix/records/YYYYMMDD/`
  - uploads `snapshot`, `raw` payload files, and clip `.mkv` files for the day
  - only processes days inside `b2.max_retentation_days`
  - deletes each local file immediately after its upload is confirmed
  - day folder cleanup happens after upload confirmation marker is uploaded
  - deletes cloud/local files older than `b2.max_retentation_days`
  - requires non-empty `b2.file_prefix` when B2 credentials are configured
- If a clip file already exists with the same name, downloader attempts resume from existing MKV duration instead of restarting from zero.
- Unlimited clip mode in manual `video-clips` command can download across devices in parallel (bounded concurrency).
- Background cycles (`run` / `run-local`) use sequential device clip pulls for lower hardware load.
- Writer commands are single-instance on host (lock file `.ramera-sync.lock`) to prevent concurrent state corruption.
- B2 operations use retry with exponential backoff for transient errors.
- CLI prints live progress logs during download (device discovery, track search, and each saved file).
- Cloud sync prints live `[progress]` upload lines (snapshot/raw/clips/marker with uploaded size).
- Cloud clip uploads are guarded: only clips with a local `.complete` sidecar are uploaded; `.resume.part`/incomplete clips are skipped.
- Deferred day does not re-upload a clip with the same filename once it already exists in cloud.
- If needed, force log level explicitly with `RUST_LOG=info`.
- In cloud mode, local day files are cleaned up after confirmed upload marker.
- TLS cert validation is disabled for local probing.
