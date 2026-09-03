#!/usr/bin/env bash
# Hermetic/static checks for the release DAG, tag pin, and macOS app assembly.
# The app test runs on Linux by replacing macOS-only tools with narrow mocks;
# the real release still performs lipo/codesign/plutil plus executable/archive
# smoke checks on macos-15.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORKFLOW="$ROOT/.github/workflows/release.yml"

bash -n \
  "$ROOT/packaging/macos/package-development-app.sh" \
  "$ROOT/scripts/verify-release-tag.sh"

python3 - "$WORKFLOW" <<'PY'
import pathlib
import subprocess
import sys

try:
    import yaml
except ImportError as exc:
    raise SystemExit("PyYAML is required to validate release.yml") from exc

workflow_path = pathlib.Path(sys.argv[1])
text = workflow_path.read_text(encoding="utf-8")
document = yaml.safe_load(text)
jobs = document.get("jobs", {})
if not isinstance(jobs, dict) or not jobs:
    raise SystemExit("release.yml has no jobs mapping")

def needs(job):
    value = jobs[job].get("needs", [])
    return [value] if isinstance(value, str) else list(value)

# Every dependency must exist, and the graph must be acyclic.
for job_name in jobs:
    unknown = sorted(set(needs(job_name)) - set(jobs))
    if unknown:
        raise SystemExit(f"{job_name} has unknown needs: {unknown}")

visiting: set[str] = set()
visited: set[str] = set()

def visit(job_name):
    if job_name in visiting:
        raise SystemExit(f"release job cycle includes {job_name}")
    if job_name in visited:
        return
    visiting.add(job_name)
    for dependency in needs(job_name):
        visit(dependency)
    visiting.remove(job_name)
    visited.add(job_name)

for name in jobs:
    visit(name)

required_pinned_jobs = {"build", "build-windows", "plugin-manifest", "npm"}
for job_name in required_pinned_jobs:
    if "release-meta" not in needs(job_name):
        raise SystemExit(f"{job_name} does not depend on the immutable release-meta job")
    checkout_steps = [
        step
        for step in jobs[job_name].get("steps", [])
        if step.get("uses") == "actions/checkout@v4"
    ]
    if len(checkout_steps) != 1:
        raise SystemExit(f"{job_name} must have exactly one source checkout")
    checkout_ref = str(checkout_steps[0].get("with", {}).get("ref", ""))
    if "needs.release-meta.outputs.commit" not in checkout_ref:
        raise SystemExit(f"{job_name} checkout is not pinned to release-meta's commit")

release_actions = [
    step.get("uses", "")
    for job in jobs.values()
    for step in job.get("steps", [])
    if str(step.get("uses", "")).startswith("softprops/action-gh-release@")
]
if not release_actions or set(release_actions) != {"softprops/action-gh-release@v3"}:
    raise SystemExit(f"all release uploads must use action-gh-release@v3: {release_actions}")

# Preserve the original CLI artifact outputs and prove the first upload includes
# both CLI/app archives and both checksum sidecars.
build_outputs = jobs["build"].get("outputs", {})
expected_build_outputs = {
    "version": "${{ needs.release-meta.outputs.version }}",
    "sha256": "${{ steps.pkg.outputs.sha256 }}",
    "asset": "${{ steps.pkg.outputs.asset }}",
}
for output_name, expression in expected_build_outputs.items():
    if build_outputs.get(output_name) != expression:
        raise SystemExit(f"build output {output_name} no longer has the CLI contract")

primary_release = next(
    step
    for step in jobs["build"].get("steps", [])
    if step.get("uses") == "softprops/action-gh-release@v3"
)
uploaded_files = set(str(primary_release.get("with", {}).get("files", "")).splitlines())
expected_uploads = {
    "${{ steps.pkg.outputs.asset }}",
    "${{ steps.pkg.outputs.asset }}.sha256",
    "${{ steps.app_pkg.outputs.asset }}",
    "${{ steps.app_pkg.outputs.asset }}.sha256",
}
if uploaded_files != expected_uploads:
    raise SystemExit(f"primary release upload contract changed: {sorted(uploaded_files)}")

homebrew_formula = next(
    step
    for step in jobs["homebrew"].get("steps", [])
    if step.get("name") == "Write Formula/nova.rb"
)
if homebrew_formula.get("env", {}) != {
    "VERSION": "${{ needs.build.outputs.version }}",
    "SHA256": "${{ needs.build.outputs.sha256 }}",
    "ASSET": "${{ needs.build.outputs.asset }}",
}:
    raise SystemExit("Homebrew is no longer wired to the CLI build outputs")

plugin_manifest = next(
    step
    for step in jobs["plugin-manifest"].get("steps", [])
    if step.get("id") == "manifest"
)
plugin_script = str(plugin_manifest.get("run", ""))
for expression in (
    'VERSION="${{ needs.build.outputs.version }}"',
    'MACOS_SHA="${{ needs.build.outputs.sha256 }}"',
):
    if expression not in plugin_script:
        raise SystemExit("Bamboo manifest is no longer wired to the CLI build outputs")

required_text = (
    "nova-v${VERSION}-universal-apple-darwin.tar.gz",
    "nova-v${VERSION}-universal-apple-darwin-development-app.zip",
    "scripts/verify-release-tag.sh",
    "concurrency:",
    "Re-verify tag immediately before publishing",
    "DEVELOPMENT ONLY",
    "Developer ID",
    "notarized",
    "stapled",
)
for needle in required_text:
    if needle not in text:
        raise SystemExit(f"release.yml is missing required contract text: {needle}")

# Parse-check every bash/default-shell run block. PowerShell blocks are skipped.
for job_name, job in jobs.items():
    for index, step in enumerate(job.get("steps", [])):
        script = step.get("run")
        shell = str(step.get("shell", "bash"))
        if not isinstance(script, str) or shell.startswith("pwsh"):
            continue
        result = subprocess.run(
            ["bash", "-n"], input=script, text=True, capture_output=True
        )
        if result.returncode:
            raise SystemExit(
                f"bash syntax error in {job_name} step {index}: {result.stderr.strip()}"
            )

print(f"release workflow YAML/DAG/shell checks passed ({len(jobs)} jobs)")
PY

# The development app is additive. These three existing distribution paths
# must continue to resolve the original universal CLI tarball name.
grep -F 'nova-v${VERSION}-universal-apple-darwin.tar.gz' "$ROOT/npm/install.js" >/dev/null
grep -F 'nova-v${VERSION}-universal-apple-darwin.tar.gz' "$ROOT/packaging/plugin/generate-manifest.sh" >/dev/null
grep -E 'nova-v[0-9.]+-universal-apple-darwin\.tar\.gz' "$ROOT/packaging/homebrew/nova.rb" >/dev/null
echo "Homebrew/npm/Bamboo CLI asset contract checks passed"

# The broad Chrome debugging sidecar must remain an explicit opt-in, and it
# must be routed through Nova's audited, pinned launcher rather than invoking
# an independently mutable npm package directly from the plugin manifest.
python3 - "$ROOT/packaging/plugin/plugin.json" <<'PY'
import json
import pathlib
import sys

manifest = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
servers = {
    server.get("id"): server
    for server in manifest.get("provides", {}).get("mcp_servers", [])
}
sidecar = servers.get("nova-chrome-devtools")
if sidecar is None:
    raise SystemExit("plugin manifest omitted nova-chrome-devtools")
if sidecar.get("enabled") is not False:
    raise SystemExit("nova-chrome-devtools must remain disabled by default")
transport = sidecar.get("transport", {})
if transport != {
    "type": "stdio",
    "command": "${platform_bin}",
    "args": ["chrome-devtools"],
}:
    raise SystemExit(
        "nova-chrome-devtools must use the packaged Nova launcher: "
        f"{transport!r}"
    )

desktop = servers.get("nova")
if desktop is None or desktop.get("enabled") is not True:
    raise SystemExit("the primary Nova desktop server must remain enabled")

print("Bamboo Chrome DevTools opt-in launcher contract checks passed")
PY

TEST_DIRECTORY="$(mktemp -d)"
trap 'rm -rf "$TEST_DIRECTORY"' EXIT

# Exercise annotated-tag peeling, the Cargo version match, and immutable HEAD.
TAG_REPOSITORY="$TEST_DIRECTORY/tag-repository"
mkdir -p "$TAG_REPOSITORY"
git -C "$TAG_REPOSITORY" init -q
git -C "$TAG_REPOSITORY" config user.name "Nova release test"
git -C "$TAG_REPOSITORY" config user.email "release-test@invalid.example"
printf '[package]\nname = "fixture"\nversion = "1.2.3"\n' > "$TAG_REPOSITORY/Cargo.toml"
git -C "$TAG_REPOSITORY" add Cargo.toml
git -C "$TAG_REPOSITORY" commit -qm "fixture"
git -C "$TAG_REPOSITORY" tag -am "fixture tag" v1.2.3
TAG_REMOTE="$TEST_DIRECTORY/tag-remote.git"
git clone -q --bare "$TAG_REPOSITORY" "$TAG_REMOTE"

TAG_OBJECT="$(git -C "$TAG_REPOSITORY" rev-parse v1.2.3)"
TAG_COMMIT="$(git -C "$TAG_REPOSITORY" rev-parse 'v1.2.3^{commit}')"
META_OUTPUT="$TEST_DIRECTORY/meta-output"
GITHUB_OUTPUT="$META_OUTPUT" RELEASE_REPO_ROOT="$TAG_REPOSITORY" \
RELEASE_VERIFY_REMOTE="$TAG_REMOTE" \
  "$ROOT/scripts/verify-release-tag.sh" v1.2.3 "$TAG_OBJECT" >/dev/null
grep -Fx "tag=v1.2.3" "$META_OUTPUT" >/dev/null
grep -Fx "version=1.2.3" "$META_OUTPUT" >/dev/null
grep -Fx "commit=$TAG_COMMIT" "$META_OUTPUT" >/dev/null

printf 'newer commit\n' > "$TAG_REPOSITORY/after-tag"
git -C "$TAG_REPOSITORY" add after-tag
git -C "$TAG_REPOSITORY" commit -qm "after tag"
NEW_COMMIT="$(git -C "$TAG_REPOSITORY" rev-parse HEAD)"
if RELEASE_REPO_ROOT="$TAG_REPOSITORY" \
  "$ROOT/scripts/verify-release-tag.sh" v1.2.3 >/dev/null 2>&1; then
  echo "error: release tag verifier accepted a checkout past the tag" >&2
  exit 1
fi
git -C "$TAG_REPOSITORY" checkout -q --detach v1.2.3
if RELEASE_REPO_ROOT="$TAG_REPOSITORY" \
  "$ROOT/scripts/verify-release-tag.sh" v1.2.3 "$NEW_COMMIT" >/dev/null 2>&1; then
  echo "error: release tag verifier accepted a mismatched event commit" >&2
  exit 1
fi
# Keep the local tag at the verified commit but simulate a remote force-update.
git -C "$TAG_REPOSITORY" push -q "$TAG_REMOTE" "$NEW_COMMIT:refs/heads/force-target"
git --git-dir="$TAG_REMOTE" update-ref refs/tags/v1.2.3 "$NEW_COMMIT"
if RELEASE_REPO_ROOT="$TAG_REPOSITORY" RELEASE_VERIFY_REMOTE="$TAG_REMOTE" \
  "$ROOT/scripts/verify-release-tag.sh" v1.2.3 >/dev/null 2>&1; then
  echo "error: release tag verifier accepted a force-moved remote tag" >&2
  exit 1
fi
git -C "$TAG_REPOSITORY" tag v1.2.4
if RELEASE_REPO_ROOT="$TAG_REPOSITORY" \
  "$ROOT/scripts/verify-release-tag.sh" v1.2.4 >/dev/null 2>&1; then
  echo "error: release tag verifier accepted a Cargo version mismatch" >&2
  exit 1
fi
echo "release tag integrity checks passed"

# Assemble the complete archive with mocked macOS platform tools. The mocks do
# not weaken the real pipeline; they make the bundle layout and signing order
# regression-testable on the ordinary Linux CI runner.
MOCK_BIN="$TEST_DIRECTORY/mock-bin"
APP_OUTPUT="$TEST_DIRECTORY/app-output"
MOCK_SOURCE="$TEST_DIRECTORY/nova"
MOCK_CODESIGN_LOG="$TEST_DIRECTORY/codesign.log"
mkdir -p "$MOCK_BIN"

printf '%s\n' \
  '#!/usr/bin/env bash' \
  'case "${1:-}" in' \
  '  --version) echo "nova ${MOCK_NOVA_VERSION}" ;;' \
  '  --help) echo "mock help" ;;' \
  '  *) exit 2 ;;' \
  'esac' > "$MOCK_SOURCE"

printf '%s\n' \
  '#!/usr/bin/env bash' \
  '[[ $# -eq 4 ]]' \
  '[[ "$1" == "-verify_arch" && "$2" == "arm64" && "$3" == "x86_64" ]]' \
  '[[ -f "$4" ]]' > "$MOCK_BIN/lipo"
printf '%s\n' '#!/usr/bin/env bash' 'exit 0' > "$MOCK_BIN/plutil"
printf '%s\n' \
  '#!/usr/bin/env bash' \
  'printf "%s\n" "$*" >> "$MOCK_CODESIGN_LOG"' > "$MOCK_BIN/codesign"
printf '%s\n' \
  '#!/usr/bin/env bash' \
  'set -euo pipefail' \
  'source_path="${@: -2:1}"' \
  'destination="${@: -1}"' \
  'parent="$(dirname "$source_path")"' \
  'name="$(basename "$source_path")"' \
  '(cd "$parent" && zip -qry "$destination" "$name")' > "$MOCK_BIN/ditto"
chmod +x "$MOCK_SOURCE" "$MOCK_BIN"/*

PATH="$MOCK_BIN:$PATH" \
MOCK_NOVA_VERSION="1.2.3-beta.1+build.7" \
MOCK_CODESIGN_LOG="$MOCK_CODESIGN_LOG" \
  "$ROOT/packaging/macos/package-development-app.sh" \
  "$MOCK_SOURCE" 1.2.3-beta.1+build.7 "$APP_OUTPUT" >/dev/null

APP="$APP_OUTPUT/Nova.app"
ASSET="$APP_OUTPUT/nova-v1.2.3-beta.1+build.7-universal-apple-darwin-development-app.zip"
[[ -x "$APP/Contents/MacOS/nova" ]]
python3 - "$APP/Contents/Info.plist" <<'PY'
import pathlib
import plistlib
import sys

with pathlib.Path(sys.argv[1]).open("rb") as stream:
    info = plistlib.load(stream)
expected = {
    "CFBundleExecutable": "nova",
    "CFBundleIdentifier": "com.zenith.nova",
    "CFBundlePackageType": "APPL",
    "CFBundleShortVersionString": "1.2.3",
    "CFBundleVersion": "1.2.3",
    "LSMinimumSystemVersion": "14.0",
    "LSMultipleInstancesProhibited": True,
    "LSUIElement": True,
}
for key, value in expected.items():
    if info.get(key) != value:
        raise SystemExit(f"unexpected Info.plist {key}: {info.get(key)!r}")
if not info.get("NSAppleEventsUsageDescription"):
    raise SystemExit("Info.plist is missing NSAppleEventsUsageDescription")
if info.get("NovaReleaseVersion") != "1.2.3-beta.1+build.7":
    raise SystemExit("Info.plist did not preserve the complete release version")
PY
grep -A1 '<key>CFBundleIdentifier</key>' "$APP/Contents/Info.plist" | grep -F '<string>com.zenith.nova</string>' >/dev/null
grep -A1 '<key>CFBundleExecutable</key>' "$APP/Contents/Info.plist" | grep -F '<string>nova</string>' >/dev/null
grep -A1 '<key>LSMinimumSystemVersion</key>' "$APP/Contents/Info.plist" | grep -F '<string>14.0</string>' >/dev/null
grep -A1 '<key>LSMultipleInstancesProhibited</key>' "$APP/Contents/Info.plist" | grep -F '<true/>' >/dev/null
grep -A1 '<key>LSUIElement</key>' "$APP/Contents/Info.plist" | grep -F '<true/>' >/dev/null
grep -A1 '<key>NSAppleEventsUsageDescription</key>' "$APP/Contents/Info.plist" | grep -F 'Nova uses Apple Events' >/dev/null
! grep -F '@VERSION@' "$APP/Contents/Info.plist" >/dev/null
[[ -f "$ASSET" && -f "$ASSET.sha256" ]]
(cd "$APP_OUTPUT" && shasum -a 256 -c "$(basename "$ASSET").sha256" >/dev/null)

CODESIGN_CALL_COUNT="$(wc -l < "$MOCK_CODESIGN_LOG" | tr -d ' ')"
CODESIGN_CALL_1="$(sed -n '1p' "$MOCK_CODESIGN_LOG")"
CODESIGN_CALL_2="$(sed -n '2p' "$MOCK_CODESIGN_LOG")"
CODESIGN_CALL_3="$(sed -n '3p' "$MOCK_CODESIGN_LOG")"
[[ "$CODESIGN_CALL_COUNT" == 3 ]]
[[ "$CODESIGN_CALL_1" == *'/Nova.app/Contents/MacOS/nova' ]]
[[ "$CODESIGN_CALL_1" != *'--deep'* ]]
[[ "$CODESIGN_CALL_2" == *'/Nova.app' ]]
[[ "$CODESIGN_CALL_2" != *'--deep'* ]]
[[ "$CODESIGN_CALL_3" == '--verify --deep --strict --verbose=2 '* ]]

echo "mock Nova.app assembly/signing/archive checks passed"
