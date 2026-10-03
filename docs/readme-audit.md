# README audit — 2026-10-03

- Zenith pin and remote HEAD: `19dfeaa1e49fd186202f731c92d2f7ec9b6539af`.
- Latest public release redirect: <https://github.com/bigduu/Nova/releases/latest>
  → `v0.2.1`, tag `c36903473022d75a5f0efeba5254be9553ca5739`.
- Current source is 71 commits after that tag. `Cargo.toml` still reports
  `0.2.1`; it is not evidence of publication of current code.
- Checked with `git ls-remote origin HEAD`, `git ls-remote --tags origin`,
  `git rev-list --count v0.2.1..HEAD`, and public release HTTP redirects.
  GitHub API access was unavailable through the environment proxy.

## Capability evidence

- `src/platform/mod.rs` selects macOS/Windows native backends and a headless
  stub elsewhere. Linux compilation is not Linux desktop support.
- `src/server.rs` and `src/tools/` register the source AX-first tool surface.
- `src/main.rs` implements managed `mcp` and app connector dispatch.
- `src/chrome_devtools.rs` pins `chrome-devtools-mcp@1.8.0`, supports isolated
  headless Chrome, and requires the external Node package. This is distinct
  from `chrome/extension` plus `chrome/native-host`'s paired-page bridge.
- `packaging/plugin/plugin.json` connects Nova to Bamboo as an optional MCP
  capability, not a model or agent harness.
- `git diff v0.2.1..HEAD -- src packaging chrome` establishes these source
  additions are newer than the release. Existing release/install limitations
  have been retained and made visible before the tool list.

No native macOS/Windows UI acceptance or performance claim is made by this
README refresh. No functionality, release files, or Zenith pins were changed.

## Approved brand illustration

The user-approved nature illustration is saved at `docs/assets/nova-nature-hero.png`.
The original PNG was visually inspected and decoded, and its SHA-256 matched
the approved image package. It is a brand illustration, not a software screenshot;
the README alt text and visible caption say so. Existing source/release and
recording limits still apply. The older artwork remains in repository history
and any existing SVG asset is preserved.

- Pixels: 1672 × 941 (RGB PNG)
- Bytes: 1994655
- SHA-256: `d90c130139531de6d600c74cb880d9452ed9fb90a0d2d2cdfcb307b3eb7f736f`
