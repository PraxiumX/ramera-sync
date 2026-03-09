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
