# Popup lifecycle acceptance (#35)

Validated on macOS 26.6.2 / Apple Silicon with official Chrome for Testing
149.0.7827.54 and an isolated profile on 2026-10-02. The browser loaded an
immutable copy of all 22 tracked extension files; their SHA256 values matched
the candidate. The native host used the existing production Chrome bridge in
an isolated test broker, not the user's running Nova application.

## Actual action-popup flow

The normal toolbar action opened the popup on the owned HTTP fixture. No
pairing was pending. Clicking **Use this tab** registered that document, then
the test broker requested Pair while the same popup remained open.

| Trigger | Observed in the actual popup | Production bridge result |
| --- | --- | --- |
| New Pair request | Reviewed fixture origin/title, countdown, Pair and Deny controls appeared without reopening | Confirmation returned the exact document route |
| Pair this page | Pending controls changed to PAIRED and Release pairing | Semantic read returned the heading, Unicode field value, button and link |
| External release | Paired controls disappeared; “Pairing ended. Ask Nova for a new Pair request when ready.” | Release succeeded |
| Wait 30 seconds | Pending controls disappeared; “Pair request expired…” recovery text appeared | `pair_expired` |
| Stop only the owned test broker | “Nova.app unavailable” and reconnect guidance appeared without reopening | Native messaging disconnected |
| Restart that broker | The same popup returned to connected / no live request | Status connected; a new explicit document registration and Pair succeeded |
| Navigate the paired tab using the browser test driver | “The page changed…” appeared and the old paired controls disappeared | `document_unloaded`, no route, zero registered frames; old snapshot action rejected |
| Allow this site | Chrome displayed its real optional-permission prompt for only `nova-consent.test` | Permission grant did not pair a document |
| Revoke site access | Allowed-site controls disappeared and removal guidance appeared | Chrome permission removed |

Native accessibility observations and action-popup screenshots were captured
in the acceptance session for these transitions. The two committed images
below show profile settings and the replacement fixture document, rather than
the action-popup overlays. Chrome closes the popup when displaying its
optional-permission prompt; it was reopened after granting that permission.
During the test-driver navigation the popup remained open and updated.

![Owned profile: Developer mode on, file/incognito access off](assets/popup-profile-boundary.png)

![Replacement fixture document after navigation](assets/popup-navigation-fixture.png)

The command-line unpacked-extension launch initially enabled Chrome's file
access checkbox. It was explicitly turned off in this owned test profile;
incognito stayed off. These settings do not assert a command-line installation
default. Daily Nova and Bodhi processes retained their original PIDs/start
times, and no macOS privacy permission was changed.

## Automated async coverage

`npm run check` and `npm test` passed locally on Node 26.10.0: 156 tests,
zero failures or skips. The existing production worker/popup fixtures cover
serialized loading/actions, current versus stale errors, delayed read/action
replies, navigation/candidate invalidation, and late probe success/failure.
Repeated bootstrap registration does not send another state notification;
unchanged reads reuse the candidate without another bootstrap or polling.

The native screenshots and real bridge flow above are separate from the
hermetic async tests. This is development-extension acceptance, not production
distribution or completion of all parent #23 criteria. CI runs the same
extension checks on the configured Node 22 runner.
