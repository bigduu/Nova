# Chrome bridge process ownership (#84)

The app broker records the operating-system identity of its accepted native-host connection. Its status reply can report the host and a bounded launch-parent relationship without accepting ownership data from a page or extension response. This is one prerequisite for the canonical provider work tracked in #23.

## Contract

Successful `chrome_status` returns broker-owned `result.ownership`. A successful observation identifies the kernel peer PID, each observed process's PID/start identity and image evidence, and the supported launch shape. It records either a direct parent or one live Windows system-CMD intermediate and its parent. The broker checks these same process instances again before reporting them. Delivered non-success status results strip worker-supplied canonical ownership, including ambiguous replies that echo another action; other error data and absent/non-object payloads remain unchanged.

`ownership_unavailable` reports that the process relationship cannot currently be confirmed. Existing paired Chrome tools remain available. The existing Session controls witness lifetime; disconnect removes it and reconnect captures a new one. No additional persistence, wire action/version, authentication policy or process-discovery service is added.

`browserIdentity` and `nativeWindowAssociation` remain `unproven`. A parent image name does not authenticate Chrome. A browser process can own multiple windows and tabs; this witness does not select a DOM provider for `ax_read` or map an extension window ID to a native window handle.

## Validation

Final native-host repair checks passed: 42 tests, zero ignored, native format and strict native/ARM64 Windows all-targets Clippy. Three real-host forged-ownership regressions failed before the repair and passed afterward, covering matched error, ambiguous status and an ambiguous reply echoing a different action, with receipt delivery and route revocation preserved. Earlier root validation passed 175 tests with 22 existing ignored tests and only the unchanged clipboard round trip filtered, both owned standalone process regressions, root format/Clippy, locked all-features metadata, workflow lint and patch checks. All final exact-head CI jobs remain required; local Windows cross compilation is not runtime evidence.

The native fixture suite must build and launch the real host on private endpoints. It covers kernel PID versus `Child::id`, stable process start identity, direct and Windows CMD parent relationships, unavailable/changed instances, spoofed hello and worker metadata, and existing disconnect/reconnect behavior. A real Windows CMD regression retains a live process snapshot, exits the child with code 259, and verifies the retained query rejects that exited instance. Windows liveness uses the process object's zero-time signal state with query and synchronization access; access failure leaves ownership unavailable. Missing fixture binaries fail setup rather than skip a test.

The actual macOS procedure uses fresh isolated profiles, an immutable accepted extension, an exact candidate native host and the candidate production broker library. It compares status against independent selected-PID libproc/`ps` observations, distinguishes a second live Chrome with the same page title and URL, exercises the real pairing and receipt path, then disconnects/reconnects and closes only owned resources. Daily profiles, installed host registration, TCC, the clipboard, Bodhi and primary repository contents are preserved.

Windows runner evidence must execute both direct and live system-CMD fixtures plus the existing managed-MCP status/receipt path. Cross compilation is a separate check and does not establish Windows runtime behavior. Packaged desktop/window/page smoke remains parent #23 work.

## Fresh actual macOS result

After the terminal-status repair, fresh actual macOS acceptance used the final tested production broker library and native host. The host binary SHA256 was `1ae87b60ee7d012b473e773c111385881c17ff333100f39e136192ca7285f968`. The nine implementation-file hashes, extension's 22 tracked payload files and both repository lockfiles were unchanged through acceptance. Earlier evidence remains separately sealed; it is not substituted for this fresh run. Real Windows execution belongs to the final exact-head CI.

| Observation | PID | OS start identity | Result |
| --- | --- | --- | --- |
| Chrome A | 31588 | `[1791008034, 786837]` | Independent libproc and targeted ps evidence matched |
| Chrome B, same title/URL | 31589 | `[1791008034, 786840]` | Independent libproc and targeted ps evidence matched |
| Initial host | 33487 | `[1791008110, 409746]` | Independent libproc and targeted ps evidence matched |
| Host after release/reconnect | 50964 | `[1791008731, 847017]` | Independent libproc and targeted ps evidence matched |
| Host after paired disconnect | 55695 | `[1791008858, 468171]` | Independent libproc and targeted ps evidence matched |

Actual broker status matched the OS host/parent PID, both start tuples and canonical image-path SHA256 values. Two live Chrome processes opened the same fixture title and URL; the parent witness identified A and did not equal B. B was normally closed after preserving that live comparison, before binding A's genuine extension popup. This avoids ambiguous native UI harness selection; it is not a window-association claim.

A genuine popup confirmation paired the owned document. Actual semantic read and activation returned terminal receipts and changed only the fixture button's public label. Release kept the same live connection witness; a first reconnect after release remained unpaired. After a genuine re-pair, terminating only the independently verified owned host revoked that active pairing. Normal extension reconnect produced a different host PID/start, retained the same Chrome A parent, rejected reads until a fresh popup confirmation, then passed re-pairing and status verification. Three tabs in A still reported `nativeWindowAssociation=unproven`.

Both Chrome processes, the native host, candidate broker and HTTP fixture were normally released/closed, with their private endpoints verified closed. Primary HEADs, contents, dirty gitlinks and raw indexes were preserved. No daily profile, installed registration, TCC permission, clipboard or Bodhi change was made. The inline pairing screenshot is Chrome confirmation evidence; Nova's native virtual cursor acceptance (#70) remains separate.

Three bounded frame-timeout setup errno-22 diagnostics occurred during this run's disconnect handling. Framing/timeout code was unchanged; the diagnostic is tracked separately in #85 without a root-cause claim. Two initial pairing requests expired during root UI-harness setup; successful genuine confirmations and active-pair disconnect evidence were recorded separately. The existing popup refresh behavior (#35) required closing/reopening the popup before confirmation; no extension repair was included.

## Identity encoding

`startIdentity` is platform local: macOS `[start_seconds, start_microseconds]`, Linux `[starttime_ticks_since_boot, 0]`, Windows `[creation_FILETIME_100ns_since_1601, 0]`. Compare only on the same machine/platform. Instance equality also checks PID, observed image and parent PID.

`imagePathSha256` hashes the UTF-8 observed path without a NUL, not the binary contents. macOS canonicalizes `proc_pidpath`; Linux uses `/proc/PID/exe`; Windows uses the Win32 path returned by `QueryFullProcessImageNameW` without additional case/slash folding. Only basename and path hash are returned. Windows system-CMD selection separately compares the OS system-directory path case-insensitively; it is not a Chrome authentication policy.
