#!/usr/bin/env bash
# Refuse to replace already-published release assets.
#
# A GitHub Release is the identity consumers pin: Bamboo, Homebrew, npm and any
# plugin manifest install by URL + SHA-256. Re-uploading a same-named asset to an
# existing release silently changes those bytes, so every consumer's pinned
# checksum stops describing what actually downloads. That happened to Nova v0.3.1:
# published 2026-10-11, assets replaced 2026-10-25.
#
# `verify-release-tag.sh` already pins the SOURCE commit to the tag. This guards
# the other half — ARTIFACT identity — by proving there is nothing to overwrite
# before the first upload of a run.
#
# Run this only in the job that performs a release's FIRST upload. Jobs that
# later attach assets to the release created by that job would see their own
# legitimately published assets and must not call it.
#
# A draft release is not yet published, so resuming one is allowed.
#
# Exits: 0 = safe to publish, 1 = refused, 2 = usage/config error.

set -euo pipefail

TAG="${1:-}"
ALLOW_REPLACE=0
shift || true
for argument in "$@"; do
  case "$argument" in
    --allow-asset-replace) ALLOW_REPLACE=1 ;;
    *)
      echo "error: unknown argument: $argument" >&2
      exit 2
      ;;
  esac
done

[[ -n "$TAG" ]] || {
  echo "error: release tag is required" >&2
  exit 2
}

# `GITHUB_REPOSITORY` is set by Actions; the override keeps the check runnable
# (and testable) outside CI without depending on a local git remote.
REPOSITORY="${RELEASE_VERIFY_REPO:-${GITHUB_REPOSITORY:-}}"
[[ -n "$REPOSITORY" ]] || {
  echo "error: release repository is unknown; set RELEASE_VERIFY_REPO" >&2
  exit 2
}

refuse() {
  echo "error: $1" >&2
  shift
  for line in "$@"; do
    echo "  $line" >&2
  done
  exit 1
}

# Distinguish "no release for this tag" from "the lookup itself failed", so a
# network or auth problem can never be mistaken for a clean slate.
set +e
RESPONSE="$(gh api "repos/${REPOSITORY}/releases/tags/${TAG}" 2>&1)"
STATUS=$?
set -e

if [[ $STATUS -ne 0 ]]; then
  if grep -q "HTTP 404" <<<"$RESPONSE"; then
    echo "No release exists for $TAG; nothing to overwrite."
    exit 0
  fi
  # An unauthenticated CLI is a configuration bug, not a policy decision: say so
  # explicitly, because the fail-closed branch would otherwise look identical to
  # a genuine refusal and block every release.
  if grep -qiE "gh auth login|GH_TOKEN|GITHUB_TOKEN|authentication" <<<"$RESPONSE"; then
    refuse \
      "the release guard could not authenticate; publishing is blocked" \
      "$(grep -viE '^$' <<<"$RESPONSE" | head -1)" \
      "Set GH_TOKEN on this step (e.g. GH_TOKEN: \${{ github.token }});" \
      "the workflow's contents:write permission does not authenticate the CLI."
  fi
  # Fail closed: an unverifiable state must not authorize a publish.
  refuse \
    "could not determine whether $TAG already has a published release" \
    "${RESPONSE%%$'\n'*}" \
    "Re-run once the API is reachable; do not bypass this check."
fi

read_field() {
  python3 -c '
import json, sys
value = json.load(sys.stdin).get(sys.argv[1])
print("" if value is None else value)
' "$1" <<<"$RESPONSE"
}

DRAFT="$(read_field draft)"
ASSETS="$(python3 -c '
import json, sys
for asset in json.load(sys.stdin).get("assets") or []:
    print(asset.get("name", ""))
' <<<"$RESPONSE")"

if [[ "$DRAFT" == "True" ]]; then
  echo "Release $TAG is still a draft; resuming it is allowed."
  exit 0
fi

if [[ -z "$ASSETS" ]]; then
  echo "Release $TAG is published but has no assets; nothing to overwrite."
  exit 0
fi

if [[ $ALLOW_REPLACE -eq 1 ]]; then
  echo "warning: replacing published assets for $TAG was explicitly allowed." >&2
  echo "warning: every consumer pinned to the previous bytes is now broken." >&2
  exit 0
fi

COUNT="$(wc -l <<<"$ASSETS" | tr -d ' ')"
refuse \
  "$TAG was already published with $COUNT asset(s); refusing to replace them." \
  "Published asset bytes are pinned by consumers (Bamboo, Homebrew, npm)," \
  "so replacing them breaks every pinned checksum." \
  "Ship a new version tag instead, e.g. bump the patch version and re-release." \
  "" \
  "Existing assets:" \
  "$(sed 's/^/  /' <<<"$ASSETS")" \
  "" \
  "Deliberate replacement (breaks consumers) requires --allow-asset-replace."
