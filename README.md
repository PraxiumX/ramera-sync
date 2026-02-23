# ramera-sync

Lightweight, local-first camera recorder sync tool — fast, reliable, and hacker-friendly.

> A minimal sync service and web UI for collecting camera records, snapshots, and clips.

## Features

- Syncs camera records and clips into a local archive
- Small Rust services: `ramera-sync` (backend) and `ramera-sync-web-ui` (frontend)
- Uses local `ffmpeg`/`ffprobe` from the `ffmpeg/` folder for processing
- Configurable via `settings.conf` (kept out of the repo by `.gitignore`)

## Quick start

1. Build the backend:

```bash
cd ramera-sync
cargo build --release
```

2. Build the web UI:

```bash
cd ramera-sync-web-ui
cargo build --release
```

3. Run the backend (example):

```bash
cd ramera-sync
./target/debug/ramera-sync
```

Replace with the release binary path if you built with `--release`.

## Important files & folders

- `settings.conf` — runtime configuration (ignored by Git).
- `target/` — build artifacts (ignored by Git).
- `records/` — runtime data (ignored by Git).
- `ffmpeg/` — bundled `ffmpeg` and `ffprobe` binaries used by the service.

## Contributing

1. Fork the repo and create a topic branch.
2. Make changes, run `cargo fmt` and tests where applicable.
3. Open a PR with a short description of the change.

## License

MIT — see LICENSE.

---
