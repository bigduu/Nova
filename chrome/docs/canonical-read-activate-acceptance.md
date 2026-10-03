# Canonical paired-page read and activate

Issue #88 routes an already consented Chrome document through `ax_read` and
`ax_activate`. Select `target="paired_page"` explicitly; `read_ui` is an alias.
Omitted `target` or `target="native"` keeps the existing AX/UIA path. A simultaneous
`window` argument, including an empty string, is rejected before provider I/O.
No bridge, live pairing or connection produces an explicit MCP error.

Page output has one canonical generation and deterministic `n1`, `n2`, … IDs.
It preserves provider page/frame coverage, partial reasons, safe values, modes,
display filtering, and Unicode/node/output budgets. Only nodes with `activate`
capability are canonical-actionable. Filtering hides lines without retargeting
or renumbering retained nodes. Provider node IDs retain existing child document
provenance. CSS bounds are omitted; native marks, PID/window identity and native
browser-toolbar coverage are absent. `nativeWindowAssociation=unproven` is an
explicit limit, not a mapping claim.

A current actionable token is consumed before DOM dispatch. The cached intent
contains the provider snapshot/node and the exact broker Session/full route
captured at read dispatch. The broker checks that binding at activation dequeue.
A changed route or a new Session, even with identical serialized route fields,
rejects locally and preserves the newer pairing. Unknown, content-only and stale
IDs send no action and do not consume a newer generation. A superseded read
cannot publish. Accepted tokens never restore or replay after failure, timeout,
ambiguity or no observed effect. Run a fresh read and inspect the page after any
outcome. DOM failure never calls native activation or coordinate/pixel fallback.

## Local regression evidence

`tests/e2e_chrome_canonical.rs` calls public JSON-RPC/MCP through the production
server and broker with a terminal fixture. `peer_identity.rs` uses the production
broker and a separately built native-host subprocess. They cover projection,
selector failures, budgets, exact action IDs, generation races, one concurrent
dispatch, consumed errors/ambiguity/timeouts, route replacement and same-route
reconnect. These fixtures do not prove real Chrome behavior or window ownership.
Run the native suite with an explicitly built `NOVA_TEST_CHROME_HOST`; no skip or
installed host is an acceptable substitute. Windows cross compilation/lint is
also distinct from Windows runtime evidence.

## Exact-candidate desktop acceptance — pending

Both real macOS and Windows acceptance remain **PENDING** until root records
the final candidate source/tree, binary hashes, extension payload hashes,
commands, genuine popup consent, public MCP responses and observed page states.
Use fresh task-owned profiles and private native-host registration. Do not reuse
daily profiles or sealed historical helpers. Mac uses the independent Nova.app
service; Windows uses the existing managed `nova mcp` in the real logged-in user
session at the same integrity level as Chrome.

1. Open a paired fixture with two same-label controls and a decoy page/window.
   Use the normal extension popup **Use This Tab / Pair This Page** consent.
2. Call `ax_read(target="paired_page",mode="all")`; verify distinct canonical
   IDs, page/frame provenance, safe values and no native/global bounds or marks.
3. Make the decoy frontmost. Activate one returned ID via public MCP, then read
   fresh and verify only that paired control changed with the existing observed
   effect result. The decoy must remain unchanged.
4. Exercise DOM replacement, navigation, release/re-pair, disconnect/reconnect
   and a no-effect control. Old IDs fail, accepted failed attempts stay consumed,
   new pairings remain usable, receipts/errors remain visible, and no fallback
   input reaches the desktop. Verify content/interactive/filter/Unicode caps.
5. On Mac record the unchanged native-target `permission_denied`. This page-only
   path uses existing Chrome consent and requires no Accessibility or Screen
   Recording grant; native AX/input and pixel capture retain their own TCC needs.

Parent #23's automatic native-window association, mixed browser chrome/page
snapshot and trusted-input routing remain separate. This slice changes no
extension permissions/protocol, registration, signing, platform resolver or
native mark compatibility contract.
