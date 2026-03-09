#!/usr/bin/env bash
set -euo pipefail

APP_NAME="ramera-sync"

usage() {
  cat <<'EOF'
Build and package a deployable folder for target machines.

Usage:
  scripts/build_deploy_bundle.sh [options]

Options:
  --output-dir <path>        Custom output folder (default: dist/ramera-sync-<utc-timestamp>)
  --no-local-config          Do not include current settings.conf and camera-filter.conf
  --skip-build               Skip cargo build step (uses existing target/release binary)
  -h, --help                 Show help

Examples:
  scripts/build_deploy_bundle.sh
  scripts/build_deploy_bundle.sh --no-local-config
  scripts/build_deploy_bundle.sh --output-dir dist/my-target-package
EOF
}

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

OUTPUT_DIR=""
INCLUDE_LOCAL_CONFIG="true"
SKIP_BUILD="false"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --output-dir)
      OUTPUT_DIR="${2:-}"
      if [[ -z "$OUTPUT_DIR" ]]; then
        echo "error: --output-dir requires a value" >&2
        exit 2
      fi
      shift 2
      ;;
    --skip-build)
      SKIP_BUILD="true"
      shift
      ;;
    --no-local-config)
      INCLUDE_LOCAL_CONFIG="false"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -z "$OUTPUT_DIR" ]]; then
  TS="$(date -u +%Y%m%dT%H%M%SZ)"
  OUTPUT_DIR="dist/${APP_NAME}-${TS}"
fi

if [[ "$SKIP_BUILD" != "true" ]]; then
  echo "[1/5] Building release binary..."
  cargo build --release
else
  echo "[1/5] Skipping build (--skip-build)"
fi

BIN_PATH="target/release/${APP_NAME}"
if [[ ! -x "$BIN_PATH" ]]; then
  echo "error: release binary not found at ${BIN_PATH}" >&2
  echo "run without --skip-build or build manually: cargo build --release" >&2
  exit 1
fi

echo "[2/5] Creating deploy folder: ${OUTPUT_DIR}"
mkdir -p "${OUTPUT_DIR}/scripts"

echo "[3/5] Copying required runtime files..."
cp "$BIN_PATH" "${OUTPUT_DIR}/${APP_NAME}"
chmod +x "${OUTPUT_DIR}/${APP_NAME}"
cp README.md "${OUTPUT_DIR}/README.md"
cp settings.conf.example "${OUTPUT_DIR}/settings.conf.example"
cp scripts/install_ffmpeg.sh "${OUTPUT_DIR}/scripts/install_ffmpeg.sh"
chmod +x "${OUTPUT_DIR}/scripts/install_ffmpeg.sh"

if [[ -f "${ROOT_DIR}/../LICENSE" ]]; then
  cp "${ROOT_DIR}/../LICENSE" "${OUTPUT_DIR}/LICENSE"
fi

if [[ "$INCLUDE_LOCAL_CONFIG" == "true" ]]; then
  echo "  - including local project configs when present"
  if [[ -f settings.conf ]]; then
    cp settings.conf "${OUTPUT_DIR}/settings.conf"
  else
    echo "  - local settings.conf not found, using template only"
  fi
  if [[ -f camera-filter.conf ]]; then
    cp camera-filter.conf "${OUTPUT_DIR}/camera-filter.conf"
  else
    cat > "${OUTPUT_DIR}/camera-filter.conf" <<'EOF'
# Device format: ip | enabled(true/false) | friendly_name
# Track format:  ip | track101 | enabled(true/false) | friendly_name | status
# status is informational only: active / no_records / unknown
EOF
  fi
else
  cat > "${OUTPUT_DIR}/camera-filter.conf.example" <<'EOF'
# Device format: ip | enabled(true/false) | friendly_name
# Track format:  ip | track101 | enabled(true/false) | friendly_name | status
# status is informational only: active / no_records / unknown
EOF
fi

if [[ -x ffmpeg/ffmpeg && -x ffmpeg/ffprobe ]]; then
  echo "  - including local ffmpeg binaries"
  mkdir -p "${OUTPUT_DIR}/ffmpeg"
  cp ffmpeg/ffmpeg ffmpeg/ffprobe "${OUTPUT_DIR}/ffmpeg/"
  chmod +x "${OUTPUT_DIR}/ffmpeg/ffmpeg" "${OUTPUT_DIR}/ffmpeg/ffprobe"
fi

cat > "${OUTPUT_DIR}/run.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT_DIR"

if [[ -x "$ROOT_DIR/ffmpeg/ffmpeg" && -x "$ROOT_DIR/ffmpeg/ffprobe" ]]; then
  export FFMPEG_BIN="$ROOT_DIR/ffmpeg/ffmpeg"
  export FFPROBE_BIN="$ROOT_DIR/ffmpeg/ffprobe"
fi

run_with_default_config() {
  local has_config_arg="false"
  for arg in "$@"; do
    if [[ "$arg" == "--config" || "$arg" == "-c" ]]; then
      has_config_arg="true"
      break
    fi
  done

  local cmd="${1:-}"
  if [[ "$has_config_arg" == "false" ]]; then
    case "$cmd" in
      run|run-local|discover|sync-once|video-records|video-clips|healthcheck|test-mode)
        exec "$ROOT_DIR/ramera-sync" "$@" --config "$ROOT_DIR/settings.conf"
        ;;
    esac
  fi

  exec "$ROOT_DIR/ramera-sync" "$@"
}

install_systemd_service() {
  local service_name="${1:-ramera-sync}"
  local service_file="/etc/systemd/system/${service_name}.service"
  local run_user="${SUDO_USER:-$(id -un)}"
  local run_group
  run_group="$(id -gn "$run_user")"

  if ! command -v systemctl >/dev/null 2>&1; then
    echo "error: systemctl not found on this machine" >&2
    exit 1
  fi

  local write_cmd=(tee "$service_file")
  local systemctl_cmd=(systemctl)
  if [[ "$EUID" -ne 0 ]]; then
    write_cmd=(sudo tee "$service_file")
    systemctl_cmd=(sudo systemctl)
  fi

  cat <<UNIT | "${write_cmd[@]}" >/dev/null
[Unit]
Description=ramera-sync production sync service
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=${run_user}
Group=${run_group}
WorkingDirectory=${ROOT_DIR}
ExecStart=${ROOT_DIR}/run.sh run
Restart=always
RestartSec=5
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
UNIT

  "${systemctl_cmd[@]}" daemon-reload
  "${systemctl_cmd[@]}" enable --now "${service_name}.service"
  echo "installed and started ${service_name}.service"
  "${systemctl_cmd[@]}" status "${service_name}.service" --no-pager || true
}

case "${1:-}" in
  install-service)
    shift
    install_systemd_service "${1:-ramera-sync}"
    exit 0
    ;;
  production)
    shift
    run_with_default_config run "$@"
    ;;
  "")
    install_systemd_service "ramera-sync"
    exit 0
    ;;
esac

run_with_default_config "$@"
EOF
chmod +x "${OUTPUT_DIR}/run.sh"

cat > "${OUTPUT_DIR}/DEPLOY.md" <<'EOF'
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
EOF

echo "[4/5] Writing file list..."
find "${OUTPUT_DIR}" -maxdepth 3 -type f | sort > "${OUTPUT_DIR}/MANIFEST.txt"

echo "[5/5] Done."
echo "Package created at: ${OUTPUT_DIR}"
echo
echo "Quick start:"
echo "  cd ${OUTPUT_DIR}"
echo "  ./run.sh --help"
