#!/usr/bin/env bash
set -euo pipefail

dest_dir="${1:-ffmpeg}"
force="${2:-}"

dest_ffmpeg="${dest_dir}/ffmpeg"
dest_ffprobe="${dest_dir}/ffprobe"

if [[ -f "${dest_ffmpeg}" && "${force}" != "--force" ]]; then
  echo "ffmpeg already exists at ${dest_ffmpeg} (use --force to overwrite)"
  exit 0
fi

os="$(uname -s)"
arch="$(uname -m)"

if [[ "${os}" != "Linux" ]]; then
  echo "installer currently supports Linux only"
  exit 1
fi

case "${arch}" in
  x86_64|amd64)
    archive_url="${FFMPEG_URL:-https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-amd64-static.tar.xz}"
    ;;
  aarch64|arm64)
    archive_url="${FFMPEG_URL:-https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-linuxarm64-gpl.tar.xz}"
    ;;
  *)
    echo "unsupported architecture: ${arch}"
    exit 1
    ;;
esac

tmp_dir="$(mktemp -d)"
cleanup() {
  rm -rf "${tmp_dir}"
}
trap cleanup EXIT

archive_path="${tmp_dir}/ffmpeg.tar.xz"

echo "Downloading ffmpeg from ${archive_url}"
curl -fsSL "${archive_url}" -o "${archive_path}"

mkdir -p "${tmp_dir}/extract"
tar -xJf "${archive_path}" -C "${tmp_dir}/extract"

src_ffmpeg="$(find "${tmp_dir}/extract" -type f -name ffmpeg | head -n 1 || true)"
src_ffprobe="$(find "${tmp_dir}/extract" -type f -name ffprobe | head -n 1 || true)"

if [[ -z "${src_ffmpeg}" ]]; then
  echo "failed to locate ffmpeg binary in downloaded archive"
  exit 1
fi

mkdir -p "${dest_dir}"
cp "${src_ffmpeg}" "${dest_ffmpeg}"
chmod +x "${dest_ffmpeg}"

if [[ -n "${src_ffprobe}" ]]; then
  cp "${src_ffprobe}" "${dest_ffprobe}"
  chmod +x "${dest_ffprobe}"
fi

echo "Installed:"
echo "  ${dest_ffmpeg}"
if [[ -f "${dest_ffprobe}" ]]; then
  echo "  ${dest_ffprobe}"
fi
