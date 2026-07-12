#!/usr/bin/env bash
# Fill the committed packaging/plugin/plugin.json TEMPLATE with a real
# release's version + the real sha256 of the platform archives release.yml
# already built (reusing the `.sha256` sidecars it computes — this script
# never re-hashes anything, only threads the values through).
#
# Windows-arch decision: bamboo's plugin schema (bamboo-plugin's
# `Platform` enum / `artifacts` map) gates by OS only — "macos" / "windows"
# / "linux" — with no per-CPU-architecture key. Nova's release ships both
# x86_64-pc-windows-msvc and aarch64-pc-windows-msvc, but only ONE can be
# named under the "windows" key. This script picks x86_64 for broad
# compatibility: it also runs under Windows' built-in x64 emulation on
# ARM64 hosts, whereas an aarch64-only binary would not run on the far more
# common x64 hosts. Shipping both needs a schema follow-up (e.g. a
# "windows-arm64" platform key) — tracked in the plugin bundle README, not
# solved here.
#
# Usage:
#   generate-manifest.sh <version> <macos_sha256> <windows_sha256> <output_path>
#
#   <version>        e.g. 1.2.3 (no leading "v")
#   <macos_sha256>   sha256 of nova-v<version>-universal-apple-darwin.tar.gz
#   <windows_sha256> sha256 of nova-v<version>-x86_64-pc-windows-msvc.zip
#   <output_path>    where to write the generated plugin.json
#
# Dry-run locally, e.g.:
#   ./generate-manifest.sh 9.9.9 "$(printf 'a%.0s' {1..64})" "$(printf 'b%.0s' {1..64})" /tmp/plugin.json

set -euo pipefail

if [ "$#" -ne 4 ]; then
  echo "usage: $0 <version> <macos_sha256> <windows_sha256> <output_path>" >&2
  exit 1
fi

VERSION="$1"
MACOS_SHA256="$2"
WINDOWS_SHA256="$3"
OUTPUT_PATH="$4"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEMPLATE="$SCRIPT_DIR/plugin.json"

if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+].*)?$ ]]; then
  echo "error: version '$VERSION' is not a plausible semver (N.N.N[-pre][+build])" >&2
  exit 1
fi

for sha in "$MACOS_SHA256" "$WINDOWS_SHA256"; do
  if ! [[ "$sha" =~ ^[0-9a-f]{64}$ ]]; then
    echo "error: sha256 '$sha' is not 64 lowercase hex chars" >&2
    exit 1
  fi
done

MACOS_ASSET="nova-v${VERSION}-universal-apple-darwin.tar.gz"
WINDOWS_ASSET="nova-v${VERSION}-x86_64-pc-windows-msvc.zip"
BASE_URL="https://github.com/bigduu/Nova/releases/download/v${VERSION}"

mkdir -p "$(dirname "$OUTPUT_PATH")"

jq \
  --arg version "$VERSION" \
  --arg macos_url "${BASE_URL}/${MACOS_ASSET}" \
  --arg macos_sha "$MACOS_SHA256" \
  --arg windows_url "${BASE_URL}/${WINDOWS_ASSET}" \
  --arg windows_sha "$WINDOWS_SHA256" \
  '.version = $version
   | .artifacts.macos.url = $macos_url
   | .artifacts.macos.sha256 = $macos_sha
   | .artifacts.windows.url = $windows_url
   | .artifacts.windows.sha256 = $windows_sha' \
  "$TEMPLATE" > "$OUTPUT_PATH"

echo "Generated $OUTPUT_PATH (version=$VERSION, macos_sha256=$MACOS_SHA256, windows_sha256=$WINDOWS_SHA256)"
