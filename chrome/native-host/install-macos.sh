#!/bin/sh
set -eu

extension_id=${1:-}
case "$extension_id" in
  ""|*[!a-p]*)
    echo "usage: $0 <32-character Chrome extension ID>" >&2
    exit 2
    ;;
esac
if [ "${#extension_id}" -ne 32 ]; then
  echo "extension ID must contain exactly 32 characters (a-p)" >&2
  exit 2
fi

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
host_path="$script_dir/target/release/nova-chrome-host"
template="$script_dir/manifest/com.zenith.nova.chrome.json.in"
install_dir="$HOME/Library/Application Support/Google/Chrome/NativeMessagingHosts"
manifest="$install_dir/com.zenith.nova.chrome.json"

if [ ! -x "$host_path" ]; then
  echo "build the host first: cargo build --release --manifest-path $script_dir/Cargo.toml" >&2
  exit 1
fi

escaped_host=$(printf '%s' "$host_path" | sed 's/[&|\\]/\\&/g')
mkdir -p "$install_dir"
umask 077
temporary="$manifest.tmp.$$"
trap 'rm -f "$temporary"' EXIT HUP INT TERM
sed \
  -e "s|__NOVA_CHROME_HOST_PATH__|$escaped_host|g" \
  -e "s|__NOVA_EXTENSION_ID__|$extension_id|g" \
  "$template" > "$temporary"
chmod 600 "$temporary"
mv -f "$temporary" "$manifest"
trap - EXIT HUP INT TERM
echo "Installed $manifest"
