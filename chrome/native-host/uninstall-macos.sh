#!/bin/sh
set -eu

manifest="$HOME/Library/Application Support/Google/Chrome/NativeMessagingHosts/com.zenith.nova.chrome.json"
if [ -f "$manifest" ]; then
  rm -f "$manifest"
  echo "Removed $manifest"
else
  echo "Nova Chrome native-host manifest is not installed"
fi
