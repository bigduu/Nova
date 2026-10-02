# Read-only child-frame acceptance (#76)

This bounded slice reads visible same-origin child documents only when Chrome's
frame/document/parent identities and the actual isolated-world document-root
iframe owner chain agree. Zero-area owners are hidden. Chrome's synchronous
`dom.openOrClosedShadowRoot` check excludes every shadow host in the owner ancestry,
including closed slots whose `assignedSlot` is null. Open-shadow owners and observed slot ancestry remain unsupported; closed-shadow,
hidden, sensitive, sandboxed, opaque, restricted and cross-origin ancestry are
excluded. A return to the top origin through a cross-origin ancestor is excluded.
Partial coverage contains bounded reason codes, never excluded child text, titles
or URLs. This does not complete #23's full-frame or exact-frame activation criteria.

Enable the exact web tab and pair it, then use **Enable read-only child frames**
in the popup. If Chrome closes the popup while granting permission, reopen Nova
and enable reads for this pairing; a retained permission alone never enables them.
`webNavigation` is optional and browser-wide in capability; Nova
queries only the paired tab. Denial leaves top-only reads and actions available.
The opt-in belongs to the current pairing epoch, clears on release, paired-page
navigation or worker restart, and is revoked by permission removal. A retained
Chrome permission never restores the volatile opt-in automatically.

Reads use one ten-second absolute content deadline, 10,000 visits, the existing
node/complete-JSON-character/UTF-8 limits, at most eight documents and depth four.
Output is root first, then preorder with ascending sibling browser frame IDs.
Child IDs use `child:<frameId>:<documentId>:<nodeId>`; children have no action
handles or bounds. All mutations on that reserved namespace fail before top
dispatch. The root snapshot still authorizes one top mutation. Inventoried child
navigation during aggregation discards child data and leaves root handles intact.

Run local gates in `chrome/extension`: `npm test`, `npm run check`, then
`git diff --check`. Hermetic tests execute packaged runtime/content scripts and
cover owner exclusions without content reads, deterministic same-URL/nested
identities, optional consent, races, shared budgets and an actual top action.

For owned browser acceptance, run `node chrome/fixtures/readonly-child-frames.mjs`
and open its printed `/top` URL in a private profile. The base case has same-URL
siblings, nested same-origin leaves, cross-origin/returned-origin ancestry,
opaque sandbox and a closed-shadow owner. Separate `?case=hidden`, `sensitive`,
`sandbox`, `opaque`, `open`, `closed`, `closed-slot`, `zero`, `documents`, `depth` and `budget` cases keep
exclusions and limits independently visible. Exercise popup denial, grant,
release/navigation/removal/restart, verify no `DO_NOT_LEAK_` child marker reaches
Nova, reject every child mutation, then increment the real top counter from the
same read. Preserve exact browser results and a rendered popup capture.

The root owns real browser/Nova acceptance and the final screenshot asset
`assets/readonly-child-frames-popup.png`; its presence alone does not prove all
acceptance cases. API references: [scripting](https://developer.chrome.com/docs/extensions/reference/api/scripting),
[webNavigation](https://developer.chrome.com/docs/extensions/reference/api/webNavigation),
[permissions](https://developer.chrome.com/docs/extensions/reference/api/permissions).
