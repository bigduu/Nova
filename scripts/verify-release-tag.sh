#!/usr/bin/env bash
# Verify that a release runner checked out exactly the requested, version-matched
# tag. When GITHUB_OUTPUT is set, publish immutable metadata for downstream jobs.

set -euo pipefail

TAG="${1:-}"
EXPECTED_EVENT_OBJECT="${2:-}"
REPOSITORY="${RELEASE_REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
REMOTE="${RELEASE_VERIFY_REMOTE:-}"

[[ -n "$TAG" ]] || {
  echo "error: release tag is required" >&2
  exit 2
}
[[ "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]] || {
  echo "error: release tag is not a supported version tag: $TAG" >&2
  exit 1
}

git -C "$REPOSITORY" show-ref --verify --quiet "refs/tags/$TAG" || {
  echo "error: tag does not exist in the checkout: $TAG" >&2
  exit 1
}

TAG_COMMIT="$(git -C "$REPOSITORY" rev-parse "refs/tags/$TAG^{commit}")"
HEAD_COMMIT="$(git -C "$REPOSITORY" rev-parse 'HEAD^{commit}')"
[[ "$TAG_COMMIT" == "$HEAD_COMMIT" ]] || {
  echo "error: checkout HEAD $HEAD_COMMIT is not the commit pinned by $TAG ($TAG_COMMIT)" >&2
  exit 1
}

if [[ -n "$EXPECTED_EVENT_OBJECT" ]]; then
  EVENT_COMMIT="$(git -C "$REPOSITORY" rev-parse "$EXPECTED_EVENT_OBJECT^{commit}")"
  [[ "$EVENT_COMMIT" == "$TAG_COMMIT" ]] || {
    echo "error: event object resolves to $EVENT_COMMIT, but $TAG resolves to $TAG_COMMIT" >&2
    exit 1
  }
fi

# Optional last-mile check for the publish job: compare against the tag as the
# remote advertises it now, peeling annotated tags to their commit. Downstream
# builds still use TAG_COMMIT, never the mutable tag name.
if [[ -n "$REMOTE" ]]; then
  REMOTE_REFS="$(git -C "$REPOSITORY" ls-remote --exit-code "$REMOTE" \
    "refs/tags/$TAG" "refs/tags/$TAG^{}")" || {
    echo "error: remote $REMOTE does not advertise tag $TAG" >&2
    exit 1
  }
  REMOTE_COMMIT="$(printf '%s\n' "$REMOTE_REFS" | awk -v ref="refs/tags/$TAG^{}" '$2 == ref { print $1; exit }')"
  if [[ -z "$REMOTE_COMMIT" ]]; then
    REMOTE_COMMIT="$(printf '%s\n' "$REMOTE_REFS" | awk -v ref="refs/tags/$TAG" '$2 == ref { print $1; exit }')"
  fi
  [[ "$REMOTE_COMMIT" == "$TAG_COMMIT" ]] || {
    echo "error: remote $REMOTE now resolves $TAG to $REMOTE_COMMIT, expected $TAG_COMMIT" >&2
    exit 1
  }
fi

CARGO_VERSION="$(sed -nE 's/^version[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/p' "$REPOSITORY/Cargo.toml" | head -1)"
[[ -n "$CARGO_VERSION" ]] || {
  echo "error: could not read package version from Cargo.toml" >&2
  exit 1
}
[[ "$TAG" == "v$CARGO_VERSION" ]] || {
  echo "error: tag $TAG does not match Cargo.toml version $CARGO_VERSION" >&2
  exit 1
}

if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
  {
    printf 'tag=%s\n' "$TAG"
    printf 'version=%s\n' "$CARGO_VERSION"
    printf 'commit=%s\n' "$TAG_COMMIT"
  } >> "$GITHUB_OUTPUT"
fi

echo "verified release tag $TAG at $TAG_COMMIT"
