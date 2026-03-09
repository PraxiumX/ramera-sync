# ramera-sync

`ramera-sync` is a Rust CLI for:
- discovering NVR devices on LAN
- collecting record metadata
- downloading video clips locally
- optionally uploading day-based data to Backblaze B2

Currently implemented provider: `hikvision` (`generic` fallback for discovery/records).

## Fast Setup

### Local-only (no B2)

```bash
# 1) Create config files
cargo run --release -- init-config --path settings.conf

# 2) Edit settings.conf (set scan.cidr + nvr.username + nvr.password)

# 3) Discover NVR + tracks and populate camera-filter.conf
cargo run --release -- discover --config settings.conf

# 4) Edit camera-filter.conf (enable/disable device + track lines)

# 5) Quick validation run (local only)
cargo run --release -- test-mode --config settings.conf --no-b2

# 6) Start local periodic loop
cargo run --release -- run-local --config settings.conf
```

### With B2 upload

```bash
# 1) Create config files
cargo run --release -- init-config --path settings.conf

# 2) Edit settings.conf (scan/nvr fields + b2.key_id + b2.application_key + b2.bucket_id + b2.file_prefix)
# Optional: cloud clip length (0 = full record)
# download.sync_clip_seconds=30

# Optional: upload same day data immediately
# b2.upload_lag_days=0

# 3) Discover NVR + tracks and populate camera-filter.conf
cargo run --release -- discover --config settings.conf

# 4) Edit camera-filter.conf (enable/disable device + track lines)

# 5) Validate runtime + B2 connectivity
cargo run --release -- healthcheck --config settings.conf --check-b2

# 6) One-shot sync (local save + cloud upload)
cargo run --release -- sync-once --config settings.conf

# 7) Or start periodic cloud loop
cargo run --release -- run --config settings.conf
```

## Quick Start

1. Generate config files:

```bash
cargo run -- init-config --path settings.conf
```

This creates both:
- `settings.conf`
- `camera-filter.conf`

2. Edit `settings.conf` (minimum):

```conf
scan.cidr=192.168.1.0/24
nvr.username=admin
nvr.password=YOUR_PASSWORD
download.max_chunk_size_mb=100
download.sync_clip_seconds=0
```

3. Discover devices and populate filter:

```bash
cargo run -- discover --config settings.conf
```

4. Edit `camera-filter.conf`:

```conf
# Device format: ip | enabled(true/false) | friendly_name
# Track format:  ip | track101 | enabled(true/false) | friendly_name | status
192.168.1.245 | true  | Main NVR
192.168.1.245 | track101 | true  | Front Door | active
192.168.1.245 | track102 | false | Back Yard  | no_records
```

5. Run smoke test:

```bash
cargo run -- test-mode --config settings.conf --no-b2
```

6. Run production commands:

```bash
cargo run --release -- healthcheck --config settings.conf --check-b2
cargo run --release -- sync-once --config settings.conf
# one-off override:
# cargo run --release -- sync-once --config settings.conf --sync-clip-seconds 30
# or periodic mode:
cargo run --release -- run --config settings.conf
```

## Command Cheat Sheet

```bash
# All commands
cargo run -- --help

# Discover and update camera-filter.conf
cargo run -- discover --config settings.conf

# Fetch metadata only (snapshot + raw payload files)
cargo run -- video-records --config settings.conf

# Download clips
cargo run -- video-clips --config settings.conf --days 1 --max-clips 3 --clip-seconds 10

# Full-length records (no trim)
cargo run -- video-clips --config settings.conf --days 30 --max-clips 0 --clip-seconds 0

# Local periodic mode (no B2)
cargo run -- run-local --config settings.conf

# Cloud one-shot sync
cargo run -- sync-once --config settings.conf
```

## Camera Filtering

`camera-filter.conf` controls which devices and tracks are processed.

Example:

```conf
# device line:
192.168.1.245 | true | Main NVR

# track lines:
192.168.1.245 | track101 | true  | Front Door | active
192.168.1.245 | track102 | false | Back Yard  | no_records
```

Behavior:
- created by `init-config`
- updated by `discover` (includes discovered Hikvision tracks + recent usage status)
- applied before metadata pull
- applied before clip download
- applied when restoring cloud snapshot for sync logic

Rules:
- if device line is `false`, all tracks are skipped
- if a track line is `false`, that track is skipped
- tracks without explicit lines default to `true`
- `status` is informational (it does not control filtering)
- `status=active` means at least one recent record was found in probe window (last 1 day)
- `status=no_records` means no records were found in probe window
- `status=unknown` means activity probe was not available

Important path rule:
- `camera-filter.conf` is resolved next to the `--config` file path.
- If you run with `--config /path/to/settings.conf`, filter is `/path/to/camera-filter.conf`.

## Download Chunk Size (Low-Resource Safety)

Use:

```conf
download.max_chunk_size_mb=100
# Cloud sync clip length (0 = full record)
download.sync_clip_seconds=0
```

Behavior:
- ffmpeg is started with size cap (`-fs`)
- resumed clips use **remaining** size budget, not full budget again
- if existing clip already reached max chunk size, it is skipped

This keeps final clip files bounded and avoids runaway growth on resume.

Suggested values:
- 2 GB RAM devices: `100-200`
- 4 GB RAM devices: `200-500`
- 8+ GB RAM devices: `500+`

## B2 Configuration (Optional)

Set in `settings.conf`:

```conf
b2.key_id=${B2_KEY_ID}
b2.application_key=${B2_APPLICATION_KEY}
b2.bucket_id=${B2_BUCKET_ID}
b2.file_prefix=ramera/nvr-snapshots
b2.max_retentation_days=60
b2.upload_lag_days=1
zero_log=false
```

Notes:
- `video-records` and `run-local` do not require B2 credentials.
- `run` and `sync-once` use day-based upload/finalization with `_complete.json` markers.
- Cloud clip length is controlled by `download.sync_clip_seconds` (`0` = full record).
- CLI override is available: `run --sync-clip-seconds <N>` / `sync-once --sync-clip-seconds <N>`.
- In cloud mode, completed clips can be uploaded while later clips are still downloading (pipelined).
- cloud cleanup respects `b2.max_retentation_days`.

## Output Layout

- `settings.conf`
- `camera-filter.conf`
- `records/snapshot-YYYYMMDD.json`
- `records/raw/YYYYMMDD/*.xml|json`
- `records/clips/<timestamp>/*.mkv` (`video-clips`)
- `records/clips/snapshot-<YYYYMMDD>/*.mkv` (`run-local` and `sync-once`)

## Runtime Notes

- Config format is `key=value` (not TOML).
- HTTP auth supports Basic and Digest.
- ffmpeg lookup order:
  - `FFMPEG_BIN` env var
  - `ffmpeg/ffmpeg`
  - system `ffmpeg`
- ffprobe lookup order:
  - `FFPROBE_BIN` env var
  - `ffmpeg/ffprobe`
  - system `ffprobe`
- `run` and `run-local` auto-install local `ffmpeg/ffprobe` if missing and runtime tools are unavailable.
- B2 operations use retry with exponential backoff.
- Incomplete clips (`.resume.part` or missing `.complete`) are skipped for cloud upload.
- `sync-once`/`run` perform open-day upload passes during clip download, then finalize the day at the end.
- Set `zero_log=true` in `settings.conf` to suppress tracing + `[progress]` logs.

## Troubleshooting

- No clips downloaded in `test-mode`:
  - retry; NVR RTSP sessions can be temporarily saturated
  - test with smaller `--max-clips` and shorter `--clip-seconds`
- Need per-camera filtering:
  - run `discover` first to populate `trackXXX` lines
  - set unwanted tracks to `false` in `camera-filter.conf`
  - run `video-clips` or `sync-once` again
- Filter not applied as expected:
  - verify you edited the `camera-filter.conf` next to the exact `--config` path you use
  - run `discover` again to refresh entries
- Missing dependencies:
  - run `cargo run -- healthcheck --config settings.conf`
  - install local binaries with `cargo run -- install-ffmpeg --dir ffmpeg`
