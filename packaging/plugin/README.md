# Nova bamboo plugin

This is the [bamboo](https://github.com/bigduu/bamboo) plugin bundle for
[Nova](https://github.com/bigduu/Nova) — a Computer Use MCP server that gives
an agent AX-first control of macOS and Windows: semantic Accessibility/UIA
reads and actions, mouse/keyboard, focused screenshots, and OCR fallback.

The required priority is:

1. `ax_read` for semantic labels, content, controls, values, and state;
2. fresh, single-use `ax_activate(snapshot_id,node_id)` for exact semantic
   activation (every attempt consumes its generation before provider dispatch);
3. focused OCR for rendered text missing from the semantic tree;
4. focused screenshot/zoom and raw coordinates only for visual-only state.

`read_ui` and `click_mark` remain compatibility aliases. A
`permission_denied` result must be fixed by granting Accessibility, not hidden
with a screenshot fallback.

## What's in this bundle

- `plugin.json` — the manifest bamboo's plugin installer reads. Declares the
  enabled `nova` desktop MCP server plus the disabled-by-default
  `nova-chrome-devtools` sidecar (both launch the per-platform binary bamboo
  places under `bin/<platform>/nova[.exe]`), the `nova-grounding` skill, and a
  `nova_desktop` prompt preset.
- `skills/nova-grounding/SKILL.md` — the AX-first read/action and ordered
  OCR/screenshot fallback workflow.

The `nova` binary itself is **not** in this bundle. `plugin.json`'s
`artifacts` point at the platform archives published alongside Nova's
GitHub Releases (built by `.github/workflows/release.yml`); the installer
downloads, sha256-verifies, and unpacks the one matching your OS.

The enabled desktop server runs `nova mcp`. On macOS this binary is only a
connector to the separately installed **Nova.app**; on Windows it runs the
ordinary stdio server. The optional Chrome DevTools server keeps its separate
`chrome-devtools` entrypoint.

## Optional Chrome DevTools server

`nova-chrome-devtools` runs `nova chrome-devtools`, which transparently
launches the pinned official `chrome-devtools-mcp@1.8.0` package. It is disabled
by default because it requires npm/`npx`, Node.js `^20.19.0`, `^22.12.0`, or
`>=23`, and current stable Chrome (or newer) on the host, and grants a much
broader Chrome debugging capability than Nova's desktop tools. Enable it only
when browser automation or DevTools inspection is wanted. URL allow patterns
require Chrome 149+; experimental WebMCP requires Chrome 150+. The URL patterns
guard only attached DevTools targets, not the host's complete network; use an
OS/VM sandbox for a true network boundary.

If Bamboo's GUI process cannot resolve `npx`, override the optional server's
transport arguments with the absolute path reported by `command -v npx`:

```json
"args": ["chrome-devtools", "--npx", "/absolute/path/to/npx"]
```

If the installed Bamboo version does not expose a server-argument override,
launch Bamboo with the Node/npm bin directory on `PATH` instead.

The default sidecar creates an isolated temporary Chrome profile and disables
usage collection, update checks, CrUX URL lookups, and unredacted sensitive
network headers. Attaching to the user's signed-in profile is an explicit
`--profile existing` choice and requires remote debugging to be enabled at
`chrome://inspect/#remote-debugging`; that mode can access all open windows in
the selected profile. It requires Chrome 144+, and Chrome chooses its default
profile when several are active. Nova's separately installed Secure Chrome
Bridge remains the least-privilege choice for an explicitly paired page.

The sidecar still runs as an `npx` process launched from Bamboo's process
chain. Nova dispatches it before calling Nova's desktop APIs; this is not a
promise about macOS responsible-process attribution for arbitrary child
processes.

This bundle is what a release actually publishes as
`nova-plugin-v<version>.tar.gz` (see the `plugin-manifest` job in
`release.yml`) — the committed `plugin.json` here is a TEMPLATE with
placeholder `version`/`artifacts` (a valid-shaped dummy version and sha256,
so it validates on its own), filled in with the real version and the real
checksums of that release's archives by
`packaging/plugin/generate-manifest.sh` at tag time.

## Installing

On macOS, first install the matching Nova.app development preview independently
of Bodhi and this plugin, at `/Applications/Nova.app` or
`~/Applications/Nova.app`, and open it once. See
[Nova.app installation](https://github.com/bigduu/Nova#novaapp-development-preview)
for checksum verification and installation. The app archive is
`nova-v<version>-universal-apple-darwin-development-app.zip`; the plugin installer
downloads the CLI archive only and does **not** install or update the app.
Keep the app outside Bodhi.app and the plugin directory so a Bodhi/plugin update
does not replace Nova's permission-bearing application.

Use a plugin, CLI, and app from the same current version. `v0.2.1` predates both
the managed `mcp` command and the app package; the current source/development
preview is needed until a release containing them is published. These plugin
defaults apply to the next release containing this change, and do not modify
an already installed `v0.2.1` plugin. An unavailable app causes a clear
connection error, with no fallback to running desktop tools inside Bamboo/Bodhi.
Windows needs no separate Nova.app installation.

A URL install is verified against the bundle's checksum by default — grab the
`.sha256` published next to the bundle on the release page and pass it:

```sh
# The .sha256 sidecar's hex is on the release page next to the bundle.
bamboo plugin install \
  https://github.com/bigduu/Nova/releases/download/v<version>/nova-plugin-v<version>.tar.gz \
  --sha256 <hex-from-nova-plugin-v<version>.tar.gz.sha256>
```

Installing from a URL **without** a checksum is refused (so a tampered or
wrong-URL `.tar.gz` can't be silently trusted); pass `--allow-unverified` only
if you knowingly accept that risk.

## Publisher signature (authenticity, not just integrity)

The `.sha256` above only proves the bundle wasn't corrupted in transit — it
says nothing about *who* produced it. On top of that, `nova-plugin-v<version>.tar.gz`
is signed with **ed25519**: the `plugin-manifest` job in `release.yml` signs
the exact bundle bytes with a private key held as the `NOVA_PLUGIN_SIGNING_KEY`
GitHub Actions secret, and publishes the signature as a sidecar,
`nova-plugin-v<version>.tar.gz.sig` (the raw 64-byte signature, lowercase hex
— 128 chars), next to the bundle and its checksum on the release page. The
release job also self-verifies the signature against the public key below
before publishing, so a broken/mismatched signing key fails the release
instead of shipping an unverifiable `.sig`.

The matching public key is committed at
[`signing-key.pub`](./signing-key.pub) in this same directory:

```
e3c429e1be50098b12c6f45737abf457189b668535875b5b3e2b4349be86ea59
```

That file is committed for transparency/reference only — **bamboo ships this
same key baked into its own default trusted-publishers store**, so
installing an official, signed Nova release needs no extra flags: `bamboo
plugin install` verifies the `.sig` against its built-in key automatically.
(A pre-signing release, or one cut before the secret was provisioned, has no
`.sig`; bamboo requires an explicit `--allow-unsigned` to install those.)

To trust a **third-party** (non-official) build of a plugin signed with a
different key, add that key to bamboo's own trust store yourself — bamboo's
plugin-trust layer supports registering additional trusted publisher keys
alongside its built-in defaults; see bamboo's plugin-trust documentation for
the exact command.

## macOS permissions (read this before first use)

Nova needs two TCC-gated macOS permissions:

- **Screen Recording** — for `screenshot`, `ocr`, `list_windows`.
- **Accessibility** — for `ax_read`, semantic activation, and input.

`ax_read` does not require Screen Recording. The current app preview can
request Screen Recording when it starts; granting it is needed for the capture
tools above. A future permission UI can make that request more contextual.

Grant these permissions to **Nova.app**, which runs independently through macOS
LaunchServices. The plugin's `nova mcp` process only forwards MCP bytes; it does
not initialize desktop APIs or request permissions in Bamboo/Bodhi's process
chain.

1. Open **System Settings → Privacy & Security → Accessibility**, add the
   installed Nova.app if needed, and enable it. Do the same under **Screen
   Recording** when using capture tools.
2. Retry the Nova tool. If macOS requires the application to restart, quit and
   reopen **Nova.app**, then reconnect/reload only the **Nova MCP server** in
   the client. **Bodhi's main window can stay open.** Interrupted MCP requests
   are not replayed automatically.
3. If the app cannot be found, check its installation path and open it once.
   Remove a development `NOVA_APP_SOCKET` override for normal use; an override
   deliberately disables automatic app launch. Do not switch the plugin back
   to empty arguments or grant permissions to the plugin connector as a repair.

Upgrading only Bodhi or the plugin leaves the independently installed Nova.app
as the desktop permission subject. This change establishes that process
boundary; real signed-upgrade/TCC acceptance remains release work. The app
preview is ad-hoc signed, not notarized: replacing or re-signing Nova.app itself
can still require granting permissions again. Developer ID signing and stable
Nova updates are separate prerequisites for a production upgrade experience.

Windows currently ships no Nova-side automation permission model equivalent
to macOS TCC; the Windows binary is otherwise unsigned (no Authenticode
cert yet), so Windows SmartScreen may warn on first run — expected, not a
plugin-packaging defect.

## Windows architecture note

bamboo's plugin schema gates `artifacts`/`${platform_bin}` by OS only
(`macos` / `windows` / `linux`) — there is no per-CPU-architecture key.
Nova's release ships both `x86_64-pc-windows-msvc` and
`aarch64-pc-windows-msvc` builds; this plugin's `windows` artifact points at
the **x86_64** build for broad compatibility (it also runs under Windows'
built-in x64 emulation on ARM64, whereas an ARM64-only binary would not run
on the far more common x64 hosts). Native ARM64 users get a slightly slower
emulated binary until the plugin schema grows a per-arch key (e.g.
`windows-arm64`) — tracked as a follow-up, not solved in this bundle.
