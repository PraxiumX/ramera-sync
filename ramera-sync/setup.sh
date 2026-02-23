#!/usr/bin/env bash
set -euo pipefail

APP_NAME="ramera-sync"
SERVICE_NAME="${APP_NAME}.service"
BIN_PATH="/usr/local/bin/${APP_NAME}"
CONF_DIR="/etc/${APP_NAME}"
DATA_DIR="/var/lib/${APP_NAME}"
SYSTEM_USER="${APP_NAME}"

# Safer default for production: keep source unless explicitly asked to remove.
REMOVE_SOURCE=0

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SUDO=()

log() {
  printf '[setup] %s\n' "$*"
}

die() {
  printf '[setup] error: %s\n' "$*" >&2
  exit 1
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "Missing command: $1"
}

run_root() {
  "${SUDO[@]}" "$@"
}

usage() {
  cat <<'EOF'
Usage: ./setup.sh [options]

Options:
  --keep-source     Keep source code after install/build (default)
  --remove-source   Remove source code after install/build
  -h, --help        Show this help
EOF
}

parse_args() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --keep-source)
        REMOVE_SOURCE=0
        shift
        ;;
      --remove-source)
        REMOVE_SOURCE=1
        shift
        ;;
      -h|--help)
        usage
        exit 0
        ;;
      *)
        die "Unknown argument: $1"
        ;;
    esac
  done
}

apt_install_system_deps() {
  if ! command -v apt-get >/dev/null 2>&1; then
    die "This script currently supports apt-based systems only."
  fi
  log "Installing system dependencies..."
  run_root apt-get update
  run_root apt-get install -y \
    build-essential \
    pkg-config \
    curl \
    ca-certificates \
    ffmpeg
}

ensure_rust_toolchain() {
  if ! command -v cargo >/dev/null 2>&1; then
    log "Installing Rust toolchain (rustup)..."
    need_cmd curl
    curl https://sh.rustup.rs -sSf | sh -s -- -y --profile minimal
  fi

  if [[ -f "${HOME}/.cargo/env" ]]; then
    # shellcheck disable=SC1090
    source "${HOME}/.cargo/env"
  fi

  need_cmd cargo
  if command -v rustup >/dev/null 2>&1; then
    rustup toolchain install stable >/dev/null
    rustup default stable >/dev/null
  fi
}

build_release() {
  log "Building release binary..."
  cd "${SCRIPT_DIR}"
  cargo build --release --locked
  [[ -x "${SCRIPT_DIR}/target/release/${APP_NAME}" ]] || die "Release binary not found after build."
}

create_system_user() {
  if ! id -u "${SYSTEM_USER}" >/dev/null 2>&1; then
    log "Creating system user ${SYSTEM_USER}..."
    run_root useradd \
      --system \
      --create-home \
      --home-dir "${DATA_DIR}" \
      --shell /usr/sbin/nologin \
      "${SYSTEM_USER}"
  fi
}

install_binary_and_config() {
  log "Installing binary and runtime directories..."
  run_root mkdir -p "${CONF_DIR}" "${DATA_DIR}"
  run_root install -m 0755 "${SCRIPT_DIR}/target/release/${APP_NAME}" "${BIN_PATH}"

  if [[ -f "${SCRIPT_DIR}/settings.conf" ]]; then
    if [[ -f "${CONF_DIR}/settings.conf" ]]; then
      log "Keeping existing ${CONF_DIR}/settings.conf"
      run_root install -m 0640 "${SCRIPT_DIR}/settings.conf" "${CONF_DIR}/settings.conf.example"
      log "Wrote example config to ${CONF_DIR}/settings.conf.example"
    else
      run_root install -m 0640 "${SCRIPT_DIR}/settings.conf" "${CONF_DIR}/settings.conf"
    fi
  elif [[ ! -f "${CONF_DIR}/settings.conf" ]]; then
    log "No local settings.conf found; generating default config..."
    run_root "${BIN_PATH}" init-config --path "${CONF_DIR}/settings.conf"
    run_root chmod 0640 "${CONF_DIR}/settings.conf"
  fi

  if [[ ! -f "${CONF_DIR}/env" ]]; then
    log "Creating ${CONF_DIR}/env template (optional env overrides)..."
    run_root tee "${CONF_DIR}/env" >/dev/null <<'EOF'
# Optional environment overrides for ramera-sync service.
# Example:
# B2_KEY_ID=...
# B2_APPLICATION_KEY=...
# B2_BUCKET_ID=...
# FFMPEG_BIN=/usr/bin/ffmpeg
# FFPROBE_BIN=/usr/bin/ffprobe
EOF
    run_root chmod 0640 "${CONF_DIR}/env"
  fi

  run_root chown -R "${SYSTEM_USER}:${SYSTEM_USER}" "${DATA_DIR}"
  run_root chown root:"${SYSTEM_USER}" "${CONF_DIR}/settings.conf" || true
  run_root chmod 0640 "${CONF_DIR}/settings.conf" || true
  run_root chown root:"${SYSTEM_USER}" "${CONF_DIR}/settings.conf.example" || true
  run_root chmod 0640 "${CONF_DIR}/settings.conf.example" || true
  run_root chown root:"${SYSTEM_USER}" "${CONF_DIR}/env" || true
  run_root chmod 0640 "${CONF_DIR}/env" || true
}

install_systemd_unit() {
  local tmp
  tmp="$(mktemp)"
  cat >"${tmp}" <<EOF
[Unit]
Description=ramera-sync service
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=${SYSTEM_USER}
Group=${SYSTEM_USER}
WorkingDirectory=${DATA_DIR}
ExecStart=${BIN_PATH} run --config ${CONF_DIR}/settings.conf
EnvironmentFile=-${CONF_DIR}/env
Restart=on-failure
RestartSec=5
StartLimitIntervalSec=0
Environment=RUST_LOG=info
UMask=027
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
MemoryDenyWriteExecute=true
ReadWritePaths=${DATA_DIR}
ReadOnlyPaths=${CONF_DIR}

[Install]
WantedBy=multi-user.target
EOF

  log "Installing systemd unit..."
  run_root install -m 0644 "${tmp}" "/etc/systemd/system/${SERVICE_NAME}"
  rm -f "${tmp}"
  run_root systemctl daemon-reload
  run_root systemctl enable --now "${SERVICE_NAME}"
}

remove_source_tree() {
  [[ "${REMOVE_SOURCE}" -eq 1 ]] || return 0

  log "Removing source code from ${SCRIPT_DIR}..."
  [[ -f "${SCRIPT_DIR}/Cargo.toml" ]] || die "Safety check failed: Cargo.toml missing; refusing source removal."
  [[ -d "${SCRIPT_DIR}/src" ]] || die "Safety check failed: src directory missing; refusing source removal."

  rm -rf \
    "${SCRIPT_DIR}/src" \
    "${SCRIPT_DIR}/scripts" \
    "${SCRIPT_DIR}/target" \
    "${SCRIPT_DIR}/Cargo.toml" \
    "${SCRIPT_DIR}/Cargo.lock" \
    "${SCRIPT_DIR}/README.md" \
    "${SCRIPT_DIR}/ffmpeg" \
    "${SCRIPT_DIR}/.git" \
    "${SCRIPT_DIR}/.gitignore"
}

main() {
  parse_args "$@"
  if [[ "${EUID}" -eq 0 ]]; then
    SUDO=()
  else
    need_cmd sudo
    SUDO=(sudo)
  fi
  apt_install_system_deps
  ensure_rust_toolchain
  build_release
  create_system_user
  install_binary_and_config
  install_systemd_unit
  remove_source_tree

  log "Done."
  log "Binary: ${BIN_PATH}"
  log "Config: ${CONF_DIR}/settings.conf"
  log "Data dir: ${DATA_DIR}"
  if [[ "${#SUDO[@]}" -gt 0 ]]; then
    log "Service: sudo systemctl status ${SERVICE_NAME}"
  else
    log "Service: systemctl status ${SERVICE_NAME}"
  fi
}

main "$@"
