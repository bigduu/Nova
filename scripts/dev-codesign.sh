#!/usr/bin/env bash
#
# Stable local code-signing for nova so macOS TCC grants (Screen Recording,
# Accessibility) PERSIST across rebuilds.
#
# Why: `cargo build` produces an ad-hoc, linker-signed binary whose identity is a
# content hash (e.g. `nova-812d733e04af2fcc`). TCC keys on that identity, so every
# rebuild looks like a brand-new app and you have to re-grant Screen Recording +
# Accessibility each time. Signing with a STABLE self-signed cert + a fixed
# identifier (`com.zenith.nova`) gives the binary a stable "designated
# requirement", so a grant you give once keeps applying to every later build.
#
# Usage:
#   ./scripts/dev-codesign.sh            # sign target/release/nova (+ debug if present)
#   ./scripts/dev-codesign.sh --release  # only release
#
# Run it AFTER each `cargo build`. The first run creates the signing identity in
# your login keychain (you may get one "codesign wants to sign using key …"
# prompt — click Always Allow). After signing the first time, grant nova once in
# System Settings → Privacy & Security → Screen Recording (and Accessibility);
# subsequent rebuilds re-sign with the same cert and the grant sticks.

set -euo pipefail

IDENTITY_NAME="Zenith Nova Code Signing"
BUNDLE_ID="com.zenith.nova"
KEYCHAIN="${HOME}/Library/Keychains/login.keychain-db"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

create_identity() {
  echo "Creating self-signed code-signing identity '${IDENTITY_NAME}' …"
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "${tmp}"' RETURN

  cat >"${tmp}/req.cfg" <<EOF
[req]
distinguished_name = dn
x509_extensions = v3
prompt = no
[dn]
CN = ${IDENTITY_NAME}
[v3]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
EOF

  openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
    -keyout "${tmp}/key.pem" -out "${tmp}/cert.pem" -config "${tmp}/req.cfg" >/dev/null 2>&1

  # Import the private key and certificate SEPARATELY (the keychain auto-links
  # them into one identity). This avoids PKCS#12 — OpenSSL 3 writes a .p12 MAC
  # that Apple's `security import` rejects ("MAC verification failed"). Pre-
  # authorise codesign to use the key.
  security import "${tmp}/key.pem" -k "${KEYCHAIN}" -T /usr/bin/codesign >/dev/null
  security import "${tmp}/cert.pem" -k "${KEYCHAIN}" -T /usr/bin/codesign >/dev/null
  # Best-effort: avoid the per-use key-access prompt. Needs the login keychain
  # password; if it isn't supplied non-interactively this is skipped and you'll
  # simply get a one-time "Always Allow" prompt on the first signature.
  security set-key-partition-list -S apple-tool:,apple: -k "" "${KEYCHAIN}" >/dev/null 2>&1 || true
}

sign() {
  local bin="$1"
  [ -f "${bin}" ] || return 0
  # Resolve a SPECIFIC identity hash — signing by name fails "ambiguous" if more
  # than one cert with this CN exists (e.g. leftover duplicates). First match wins.
  local hash
  hash="$(security find-identity -p codesigning 2>/dev/null | awk -v n="${IDENTITY_NAME}" '$0 ~ n {print $2; exit}')"
  [ -n "${hash}" ] || hash="${IDENTITY_NAME}"
  codesign --force --identifier "${BUNDLE_ID}" --sign "${hash}" "${bin}"
  echo "signed: ${bin}  ($(codesign -dv "${bin}" 2>&1 | awk -F= '/^Identifier=/{print $2}'))"
}

# Create the identity once. NB: use `find-certificate` (not `find-identity -v`)
# — a self-signed cert is not policy-"valid", so `-v` would never list it and we
# would create a duplicate on every run (which makes codesign fail "ambiguous").
if ! security find-certificate -c "${IDENTITY_NAME}" >/dev/null 2>&1; then
  create_identity
fi

case "${1:-}" in
  --release) sign "${ROOT}/target/release/nova" ;;
  *)
    sign "${ROOT}/target/release/nova"
    sign "${ROOT}/target/debug/nova"
    ;;
esac

echo "Done. If this was the first stable-signed build, grant nova once in"
echo "System Settings → Privacy & Security → Screen Recording (and Accessibility),"
echo "then reconnect nova. Future rebuilds keep the grant."
