# Chrome bridge process ownership (#84)

The app broker records the operating-system identity of its accepted native-host connection. Its status reply can report the host and a bounded launch-parent relationship without accepting ownership data from a page or extension response. This is one prerequisite for the canonical provider work tracked in #23.

## Contract

`chrome_status` returns broker-owned `result.ownership`. A successful observation identifies the kernel peer PID, each observed process's PID/start identity and image evidence, and the supported launch shape. It records either a direct parent or one live Windows system-CMD intermediate and its parent. The broker checks these same process instances again before reporting them.

`ownership_unavailable` reports that the process relationship cannot currently be confirmed. Existing paired Chrome tools remain available. The existing Session controls witness lifetime; disconnect removes it and reconnect captures a new one. No additional persistence, wire action/version, authentication policy or process-discovery service is added.

`browserIdentity` and `nativeWindowAssociation` remain `unproven`. A parent image name does not authenticate Chrome. A browser process can own multiple windows and tabs; this witness does not select a DOM provider for `ax_read` or map an extension window ID to a native window handle.

## Validation

Local implementation checks passed: native-host 36 tests; Nova 175 tests, 22 existing ignored tests and only the unchanged clipboard round trip filtered; both owned standalone process regressions passed. Root/native format, strict all-targets Clippy, ARM64 Windows cross-Clippy, locked all-features metadata, workflow lint and patch checks passed. Windows runtime execution is required in the exact-head `windows-chrome-bridge` CI job before merge; local cross compilation is not that evidence.

The native fixture suite must build and launch the real host on private endpoints. It covers kernel PID versus `Child::id`, stable process start identity, direct and Windows CMD parent relationships, unavailable/changed instances, spoofed hello and worker metadata, and existing disconnect/reconnect behavior. A real Windows CMD regression retains a live process snapshot, exits the child with code 259, and verifies the retained query rejects that exited instance. Windows liveness uses the process object's zero-time signal state with query and synchronization access; access failure leaves ownership unavailable. Missing fixture binaries fail setup rather than skip a test.

The actual macOS procedure uses fresh isolated profiles, an immutable accepted extension, an exact candidate native host and the candidate production broker library. It compares status against independent selected-PID libproc/`ps` observations, distinguishes a second live Chrome with the same page title and URL, exercises the real pairing and receipt path, then disconnects/reconnects and closes only owned resources. Daily profiles, installed host registration, TCC, the clipboard, Bodhi and primary repository contents are preserved.

Windows runner evidence must execute both direct and live system-CMD fixtures plus the existing managed-MCP status/receipt path. Cross compilation is a separate check and does not establish Windows runtime behavior. Packaged desktop/window/page smoke remains parent #23 work.

## Fresh actual macOS result

The candidate production broker library and host were built from the nine tested implementation files. The host binary SHA256 was `122856512428afe386f777f96a2e3edbea132d295693506602dc747a0a619f85`. The extension's 22 tracked payload files and both repository lockfiles were unchanged. A subsequent Windows-only liveness repair and Windows regression change no compiled macOS production path; the sealed actual macOS evidence remains bound to the recorded binary and original source hashes. Real Windows execution belongs to the final exact-head CI.

| Observation | PID | OS start identity | Result |
| --- | --- | --- | --- |
| Chrome A | 66175 | `[1791001920, 801018]` | Independent libproc and targeted ps evidence matched |
| Chrome B, same title/URL | 27441 | `[1791000303, 225815]` | Independent libproc and targeted ps evidence matched |
| Initial host | 68119 | `[1791002007, 520979]` | Independent libproc and targeted ps evidence matched |
| Reconnected host | 78322 | `[1791002473, 514417]` | Independent libproc and targeted ps evidence matched |

Actual broker status matched the OS host/parent PID, both start tuples and canonical image-path SHA256 values. Two live Chrome processes opened the same fixture title and URL; the parent witness identified A and did not equal B. The native UI harness initially selected older B, so B was normally closed after preserving that live comparison, before binding A's genuine extension popup. This was a harness selection limit, not a window-association claim.

A genuine popup confirmation paired the owned document. Actual semantic read and activation returned terminal receipts and changed only the fixture button's public label. Release kept the same live connection witness. Terminating only the independently verified owned host revoked pairing; the normal extension reconnect produced a different host PID/start, retained the same Chrome A parent, and required a fresh popup confirmation. Re-pairing and status verification passed. Three tabs in A still reported `nativeWindowAssociation=unproven`.

Both Chrome processes, the native host, candidate broker and HTTP fixture were normally released/closed, with their private endpoints verified closed. Primary HEADs, contents, dirty gitlinks and raw indexes were preserved. No daily profile, installed registration, TCC permission, clipboard or Bodhi change was made. The inline pairing screenshot is Chrome confirmation evidence; Nova's native virtual cursor acceptance (#70) remains separate.

Two bounded frame-timeout setup errno-22 diagnostics occurred during disconnect handling. Independent review verified that framing/timeout code was unchanged; the diagnostic is tracked separately in #85 without a root-cause claim. The existing popup refresh behavior (#35) required closing/reopening the popup before the first confirmation; no extension repair was included.

## Identity encoding

`startIdentity` is platform local: macOS `[start_seconds, start_microseconds]`, Linux `[starttime_ticks_since_boot, 0]`, Windows `[creation_FILETIME_100ns_since_1601, 0]`. Compare only on the same machine/platform. Instance equality also checks PID, observed image and parent PID.

`imagePathSha256` hashes the UTF-8 observed path without a NUL, not the binary contents. macOS canonicalizes `proc_pidpath`; Linux uses `/proc/PID/exe`; Windows uses the Win32 path returned by `QueryFullProcessImageNameW` without additional case/slash folding. Only basename and path hash are returned. Windows system-CMD selection separately compares the OS system-directory path case-insensitively; it is not a Chrome authentication policy.
