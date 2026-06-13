# @bigduu/nova

npx launcher for **Nova**, a Computer Use MCP server for macOS (screenshots,
mouse, keyboard, window management, app introspection).

This package downloads the prebuilt **universal** macOS binary for its version
from Nova's GitHub Releases on install, then launches it — passing stdio
through transparently so it works as an MCP stdio server.

> macOS only. Nova drives ScreenCaptureKit and the Accessibility API.

## Use it as an MCP server

```json
{
  "mcpServers": {
    "nova": { "command": "npx", "args": ["-y", "@bigduu/nova"] }
  }
}
```

Or install it on PATH:

```sh
npm install -g @bigduu/nova
# then: "command": "nova"
```

## Permissions (do this once)

Grant **Screen Recording** and **Accessibility** to the process that macOS holds
responsible for Nova. As a subprocess of an MCP host, that responsible process
is usually the **host app** (Claude Desktop) or your **terminal** — grant *that*,
not just the binary. See the
[main README](https://github.com/bigduu/Nova#permissions--code-signing-macos).

## License

MIT — see https://github.com/bigduu/Nova
