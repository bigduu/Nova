#!/usr/bin/env bash
# Assemble the ad-hoc-signed Nova.app DEVELOPMENT preview from an already-built
# universal CLI binary. This is intentionally not a production distribution
# pipeline: Developer ID signing, hardened runtime, notarization, and stapling
# remain release gates (documented in README.md).

set -euo pipefail

usage() {
  echo "usage: $0 <universal-nova-binary> <version> [output-directory]" >&2
  exit 2
}

[[ $# -ge 2 && $# -le 3 ]] || usage

SOURCE_BINARY="$1"
VERSION="$2"
OUTPUT_DIRECTORY="${3:-dist}"

[[ -f "$SOURCE_BINARY" ]] || {
  echo "error: Nova binary does not exist: $SOURCE_BINARY" >&2
  exit 1
}
[[ -x "$SOURCE_BINARY" ]] || {
  echo "error: Nova binary is not executable: $SOURCE_BINARY" >&2
  exit 1
}
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]] || {
  echo "error: invalid release version: $VERSION" >&2
  exit 1
}

for command_name in lipo codesign plutil ditto unzip shasum sips iconutil; do
  command -v "$command_name" >/dev/null 2>&1 || {
    echo "error: required command is unavailable: $command_name" >&2
    exit 1
  }
done

SCRIPT_DIRECTORY="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLIST_TEMPLATE="$SCRIPT_DIRECTORY/Info.plist"
ICON_SOURCE="$SCRIPT_DIRECTORY/../../assets/nova-app-icon.png"
[[ -f "$PLIST_TEMPLATE" ]] || {
  echo "error: missing Info.plist template: $PLIST_TEMPLATE" >&2
  exit 1
}
[[ -s "$ICON_SOURCE" ]] || {
  echo "error: missing Nova app icon source: $ICON_SOURCE" >&2
  exit 1
}

mkdir -p "$OUTPUT_DIRECTORY"
OUTPUT_DIRECTORY="$(cd "$OUTPUT_DIRECTORY" && pwd)"
APP="$OUTPUT_DIRECTORY/Nova.app"
APP_BINARY="$APP/Contents/MacOS/nova"
APP_ICON="$APP/Contents/Resources/Nova.icns"
ASSET_NAME="nova-v${VERSION}-universal-apple-darwin-development-app.zip"
ASSET="$OUTPUT_DIRECTORY/$ASSET_NAME"

# The exact targets are fixed and validated above; do not accept an arbitrary
# recursive deletion target from an environment variable or glob.
if [[ "$APP" != */Nova.app || "$APP" == /Nova.app ]]; then
  echo "error: refusing unsafe app staging path: $APP" >&2
  exit 1
fi
rm -rf "$APP"
rm -f "$ASSET" "$ASSET.sha256"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "$SOURCE_BINARY" "$APP_BINARY"
chmod 0755 "$APP_BINARY"

# Build the standard macOS icon representations from the committed 1024px
# source. Keep the intermediate iconset outside the bundle so only Nova.icns is
# sealed into the final app signature and archive.
ICON_WORK_DIRECTORY="$(mktemp -d "$OUTPUT_DIRECTORY/.nova-icon.XXXXXX")"
trap 'rm -rf "$ICON_WORK_DIRECTORY"' EXIT
ICONSET_DIRECTORY="$ICON_WORK_DIRECTORY/Nova.iconset"
mkdir -p "$ICONSET_DIRECTORY"

while read -r pixel_size icon_name; do
  sips -z "$pixel_size" "$pixel_size" "$ICON_SOURCE" \
    --out "$ICONSET_DIRECTORY/$icon_name" >/dev/null
done <<'ICON_SIZES'
16 icon_16x16.png
32 icon_16x16@2x.png
32 icon_32x32.png
64 icon_32x32@2x.png
128 icon_128x128.png
256 icon_128x128@2x.png
256 icon_256x256.png
512 icon_256x256@2x.png
512 icon_512x512.png
1024 icon_512x512@2x.png
ICON_SIZES

iconutil -c icns -o "$APP_ICON" "$ICONSET_DIRECTORY"
[[ -s "$APP_ICON" ]] || {
  echo "error: iconutil did not produce a non-empty app icon: $APP_ICON" >&2
  exit 1
}

BUNDLE_VERSION="${VERSION%%[-+]*}"
sed \
  -e "s/@VERSION@/$VERSION/g" \
  -e "s/@SHORT_VERSION@/$BUNDLE_VERSION/g" \
  -e "s/@BUNDLE_VERSION@/$BUNDLE_VERSION/g" \
  "$PLIST_TEMPLATE" > "$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist"

# Refuse to publish an accidentally thin app. Verify both the source and the
# installed copy so packaging cannot silently replace or corrupt a slice.
lipo -verify_arch arm64 x86_64 "$SOURCE_BINARY"
lipo -verify_arch arm64 x86_64 "$APP_BINARY"

# Sign inside-out, explicitly, without `codesign --deep`: sign nested code
# first and the outer bundle last. Ad-hoc signing is sufficient for this
# DEVELOPMENT preview but is not a substitute for Developer ID + notarization.
codesign --force --sign - --timestamp=none \
  --identifier com.zenith.nova "$APP_BINARY"
codesign --force --sign - --timestamp=none \
  --identifier com.zenith.nova "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"

# Exercise the exact executable placed inside the app and ensure its embedded
# Cargo version agrees with the asset label before creating the archive.
APP_VERSION_OUTPUT="$("$APP_BINARY" --version)"
grep -F -- "$VERSION" <<<"$APP_VERSION_OUTPUT" >/dev/null || {
  echo "error: app binary version does not match $VERSION: $APP_VERSION_OUTPUT" >&2
  exit 1
}
"$APP_BINARY" --help >/dev/null

ditto -c -k --sequesterRsrc --keepParent "$APP" "$ASSET"
unzip -Z1 "$ASSET" | grep -Fx 'Nova.app/Contents/MacOS/nova' >/dev/null || {
  echo "error: archive does not contain Nova.app/Contents/MacOS/nova" >&2
  exit 1
}
unzip -Z1 "$ASSET" | grep -Fx 'Nova.app/Contents/Info.plist' >/dev/null || {
  echo "error: archive does not contain Nova.app/Contents/Info.plist" >&2
  exit 1
}
unzip -Z1 "$ASSET" | grep -Fx 'Nova.app/Contents/Resources/Nova.icns' >/dev/null || {
  echo "error: archive does not contain Nova.app/Contents/Resources/Nova.icns" >&2
  exit 1
}

SHA256="$(shasum -a 256 "$ASSET" | awk '{print $1}')"
printf '%s  %s\n' "$SHA256" "$ASSET_NAME" > "$ASSET.sha256"

echo "Built DEVELOPMENT ONLY preview: $ASSET"
echo "sha256: $SHA256"
