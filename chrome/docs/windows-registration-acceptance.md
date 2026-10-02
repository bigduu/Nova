# Windows Google Chrome registration

Nova #74 supplies one current-user developer installation of an explicitly
provided, already-built `nova-chrome-host.exe`. In PowerShell:

```powershell
nova chrome-host install --host-binary C:\build\nova-chrome-host.exe --extension-id abcdefghijklmnopabcdefghijklmnop
nova chrome-host status
nova chrome-host uninstall
```

An optional `--pipe '\\.\pipe\nova-chrome-example'` selects a local Nova pipe.
The ID must be the exact 32-character extension ID (letters `a` through `p`).
Successful install/status reports the ID and matching `nova mcp` environment.
Supply that environment explicitly in the MCP client: installation does not
configure another process. Changed binaries, IDs or pipes require uninstall
and reinstall; identical owned installations are a no-op.

The current user's LocalAppData is resolved by `SHGetKnownFolderPath`.
`LocalAppData\Nova\ChromeNativeMessaging` owns only these fixed files:

- `nova-chrome-host.exe`: the copied binary;
- `com.zenith.nova.chrome.json`: an absolute-path stdio manifest with one origin;
- `nova-chrome-host.json`: a bounded JSON record with `schemaVersion: 1`,
  `owner: com.zenith.nova.chrome`, `extensionId`, `binarySha256`, optional `pipe`.

The default REG_SZ value of
`HKCU\Software\Google\Chrome\NativeMessagingHosts\com.zenith.nova.chrome`
points at the owned manifest. Files are written before publication. Operations
use the explicit 32-bit view and verify agreement through both view selectors.
Modern Windows shares this HKCU Software path; no Wow6432Node key is created.
See [Chrome's contract](https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging)
and [Microsoft's shared-key table](https://learn.microsoft.com/en-us/windows/win32/winprog64/shared-registry-keys).

Only native-host startup reads the fixed record beside `current_exe()`, and only
when both `NOVA_CHROME_EXTENSION_ID` and `NOVA_CHROME_PIPE` are absent. CWD and
registry-referenced foreign files never select configuration. An omitted pipe
computes the current user/logon/session default at each launch. Present complete
environment configuration stays authoritative; incomplete, invalid or
non-Unicode environment cannot fall back. Origin and hello must match the
resolved exact ID before messages enter the existing authenticated transport.
Managed MCP stays environment-only. Missing/malformed/unsupported records fail
with an installation/configuration diagnostic.

Status reports `absent`, `installed`, `incomplete` or `conflict` with an immediate
action and never occupies the broker. Uninstall verifies the record, binary,
manifest and default value first. It preserves replacements, unrelated files,
values, subkeys and siblings; deletes only empty owned directories/leaves; and
keeps the record for retry if a payload removal fails. Close the owned host and
retry a Windows sharing/deletion error. Reparse targets are refused. The record
is an operational current-user deletion guard, not a same-user rewrite barrier.

The Windows CI job builds both fixture binaries and supplies absolute
`NOVA_TEST_CHROME_HOST` and `NOVA_TEST_NOVA_EXE` paths. Missing setup fails.
Private unit fixtures use unique temporary directories and HKCU test subtrees;
the public CLI exposes no target override. They test actual registry values and
views, refusal/preservation, an installed copied host launched with a different
CWD and cleared Chrome environment, real `nova mcp` initialization/status/receipt,
binary-only host stdout and bounded exit while host stdin stays open. Existing
environment-driven transport/process/MCP contracts and Windows CLI parsing run
in that same job. macOS compatibility checks cannot prove Windows execution.

This source acceptance does not prove actual Chrome registry discovery, an
installed extension, packaged browser smoke, signing, upgrades or tracker #23.
