# @bigduu/nova

Node launcher for [Nova](https://github.com/bigduu/Nova), a computer-use MCP
server. **Homebrew is the recommended macOS install** (`brew install bigduu/tap/nova`).
This package is the npm entry point: on install (or on first launch, if
install scripts were skipped) it downloads the prebuilt **v0.3.1** GitHub
Release binary for the current machine and checks the published `.sha256`.

MCP Registry name: `io.github.bigduu/nova`.

```sh
npx @bigduu/nova --version
npm i -g @bigduu/nova
```

| Platform | Release asset |
| --- | --- |
| macOS 14+ (Apple Silicon and Intel) | `nova-v0.3.1-universal-apple-darwin.tar.gz` |
| Windows x64 | `nova-v0.3.1-x86_64-pc-windows-msvc.zip` |
| Windows ARM64 | `nova-v0.3.1-aarch64-pc-windows-msvc.zip` |
| Linux | not installed (no desktop-control binary in this package) |

Requires Node.js 18+. The Rust source is not published in this package.

For an MCP client, prefer a non-interactive `npx`:

```json
{
  "mcpServers": {
    "nova": { "command": "npx", "args": ["-y", "@bigduu/nova"] }
  }
}
```

The macOS binary is ad-hoc signed, not notarized; Gatekeeper can block the
first run. Windows binaries are not Authenticode-signed, so SmartScreen may
warn. Grant **Accessibility** and **Screen Recording** on macOS to the process
that launches Nova. See the [install guide](../README.md#install).
