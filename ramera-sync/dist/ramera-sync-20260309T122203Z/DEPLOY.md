# Deploy Package

This folder is portable to a target machine.

## Contents
- `ramera-sync` executable
- `settings.conf.example` template
- `settings.conf` and `camera-filter.conf` from source project (if available)
- `scripts/install_ffmpeg.sh` helper
- optional: `ffmpeg/` local binaries (if available on build machine)

## On target machine
1. Copy this whole folder.
2. Create `settings.conf` (from `settings.conf.example`) and `camera-filter.conf` in this folder.
3. Run:

```bash
./run.sh healthcheck
./run.sh discover
./run.sh run
```

## Auto-start on reboot/crash (production mode)

```bash
./run.sh install-service
```

This installs a `systemd` service that always starts with:

```bash
./run.sh run
```

Default behavior:

```bash
./run.sh
```

With no arguments, it runs `install-service` automatically.
