# Nova bamboo plugin

This is the [bamboo](https://github.com/bigduu/bamboo) plugin bundle for
[Nova](https://github.com/bigduu/Nova) — a Computer Use MCP server that gives
an agent control of the macOS (and, headless-only for now, Windows) desktop:
screenshots, mouse/keyboard, Set-of-Mark UI grounding, and on-device OCR.

## What's in this bundle

- `plugin.json` — the manifest bamboo's plugin installer reads. Declares the
  `nova` MCP server (stdio, launching the per-platform binary bamboo places
  under `bin/<platform>/nova[.exe]`), the `nova-grounding` skill, and a
  `nova_desktop` prompt preset.
- `skills/nova-grounding/SKILL.md` — the coordinate/marks/click_mark
  targeting workflow: how to reliably click the right pixel or UI element
  instead of guessing.

The `nova` binary itself is **not** in this bundle. `plugin.json`'s
`artifacts` point at the platform archives published alongside Nova's
GitHub Releases (built by `.github/workflows/release.yml`); the installer
downloads, sha256-verifies, and unpacks the one matching your OS.

This bundle is what a release actually publishes as
`nova-plugin-v<version>.tar.gz` (see the `plugin-manifest` job in
`release.yml`) — the committed `plugin.json` here is a TEMPLATE with
placeholder `version`/`artifacts` (a valid-shaped dummy version and sha256,
so it validates on its own), filled in with the real version and the real
checksums of that release's archives by
`packaging/plugin/generate-manifest.sh` at tag time.

## Installing

```sh
bamboo plugin install https://github.com/bigduu/Nova/releases/download/v<version>/nova-plugin-v<version>.tar.gz
```

(Once the CLI/HTTP install flow supports it — see bamboo's `PLUGIN_PLAN.md`.)

## macOS permissions (read this before first use)

Nova needs two TCC-gated macOS permissions:

- **Screen Recording** — for `screenshot`, `ocr`, `list_windows`.
- **Accessibility** — for posting mouse/keyboard input and Set-of-Mark.

Nova runs as an unbundled subprocess of bamboo (stdio MCP transport), so the
permission prompt and grant attach to the **launching process — Bamboo**,
not to this plugin's `nova` binary. The plugin system has no post-install
hook to request this today, so:

1. The first time a nova tool actually needs the permission, macOS should
   prompt. Grant it, then retry the tool call.
2. If the prompt never appears, or capture keeps failing/returning empty,
   add Bamboo manually: **System Settings → Privacy & Security → Screen
   Recording** (and **Accessibility**) → `+` → find Bamboo (or ⌘⇧G to enter
   its path) → enable it. A headless/backgrounded subprocess often can't
   trigger the prompt on its own.
3. If granting Bamboo still doesn't help, add the `nova` binary itself as a
   fallback: it lives at `bin/macos/nova` inside this plugin's install
   directory (`~/.bamboo/plugins/nova/bin/macos/nova`) once installed —
   add that path the same way.
4. The grant is keyed to Bamboo's **code-signing identity**, not just "the
   app." If Bamboo is rebuilt/re-signed (dev builds, ad-hoc/unsigned) the
   grant can silently stop persisting — if permissions mysteriously stop
   working after updating Bamboo, re-grant it.

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
