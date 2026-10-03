---
name: nova-grounding
description: AX-first reading and grounded activation workflow for Nova desktop automation on macOS and Windows. Use for reading UI, clicking controls, OCR fallback, screenshots, and coordinate input without guessing.
---

# Nova AX-first desktop grounding

Nova controls macOS and Windows. Treat `ax_read` as the first-class
`ax:read` capability. It reads Accessibility/UIA by default and does not take
a screenshot. `read_ui` is a compatibility alias.

## Chrome routing

When the separately enabled `nova-chrome-devtools` server is available, use
its official Chrome DevTools tools for routine web-page automation, DOM and
network inspection, and performance debugging. Use Nova for browser chrome,
permission dialogs, other desktop apps, and pixel fallback. If the user needs
least-privilege access to one explicitly paired page, prefer Nova's separately
installed Secure Chrome Bridge instead of broad existing-profile DevTools
access.

For an already consented Secure Chrome Bridge page, call
`ax_read(target="paired_page", mode="all")`, then
`ax_activate(snapshot_id, node_id)` on an activate-capable canonical node.
`target="native"` or an omitted target keeps native selection; `paired_page`
rejects `window=`. This page-only snapshot reports `provider=chrome_extension`,
page/frame coverage and `nativeWindowAssociation=unproven`. It supplies no native
marks, browser toolbar, PID/window association or global coordinates. Do not
infer ownership from titles, URLs, Chrome window IDs or the frontmost window.
Only the already paired document is eligible, even when a decoy is frontmost.

Unknown/content-only/stale page IDs dispatch nothing. An accepted current page
token consumes its generation before I/O; route changes, reconnects, DOM errors,
no observed effect, ambiguity and timeout never restore or replay it. Read fresh
and inspect before choosing another action. Keep page failures on this path;
do not switch to native AX/UIA, AppleScript, app raising, OCR or coordinate input
to retry a failed DOM action. Direct `chrome_*` tools remain available.

## Read in this order

1. Call `ax_read(window?, mode="all")` first for labels, controls, fields,
   headings, structured text, values, supported actions, and semantic state.
   It returns an ephemeral `snapshot_id`, snapshot-local node IDs, optional
   bounds, and an explicit coverage/status.
2. If coverage is absent or partial and the missing information is rendered
   text, call focused-window `ocr`. OCR returns a grounded center for each
   recognized line.
3. Use `screenshot(window=...)` or `zoom_region` only when pixels are
   semantically necessary: layout, icons, colors, images, canvas content, or
   visual verification.

Do not treat `permission_denied` as an AX-less application. Grant
Accessibility and retry; a screenshot or OCR call cannot repair the missing
permission.

## Act in this order

1. Immediately before acting, run a fresh `ax_read`, select the exact
   actionable node, and call `ax_activate(snapshot_id, node_id)`.
2. Nova attempts semantic activation first (`route=ax` on macOS,
   `route=uia` on Windows, `route=chrome_extension` for an explicitly paired page, or
   `route=web_dom` for supported native browser content).
   If that is unsupported, Nova may use the node's freshly revalidated center
   (`route=element_center`).
3. For text absent from the semantic tree, click the center returned by OCR
   with `left_click(x, y, source="ocr_center")`.
4. Only then click coordinates read from a focused screenshot or zoom
   (`source="visual_coordinate"`).

`ax_activate` fails closed when its snapshot generation is stale. An accepted current
actionable token consumes the generation before provider dispatch, including
attempts that return an error because the provider may have partially applied
the action. Run `ax_read` again after any result and after navigation, refresh,
scrolling, or any other UI change.
`click_mark(number=N)` remains a compatibility action. Prefer
generation-safe `ax_activate`. Legacy substring actions (`ax_click`,
`ax_focus`, `ax_set_value`) reject ambiguous matches instead of selecting the
first candidate.

## Verify every action

Observe the outcome before choosing the next action:

- Prefer `ax_read` when the expected result is semantic: text, selection,
  checked/enabled/focused state, or newly appearing controls.
- Use a screenshot when the expected result is visual: layout, icon, color,
  image, animation, or canvas state.
- For long content, scroll one step, read or observe it, then continue.

## Coordinate fallback

All pointer coordinates use the pixel space of the most recent screenshot.
Never guess from a downscaled full-display image. Capture one window or use
`zoom_region`, then read coordinates from the visible grid. Foreground input
is the universal default; background input is best-effort for native apps and
may be ignored by browsers or custom-rendered surfaces.

## Permissions

On macOS:

- Accessibility is required for native `ax_read`, native semantic activation,
  and native input. Explicit `paired_page` read/activate uses existing Chrome
  popup consent without an Accessibility or Screen Recording grant.
- Screen Recording is required only for pixel capture, OCR, and
  capture-backed window listing.

Nova runs as Bamboo's subprocess in the plugin. The permission grant commonly
attaches to Bamboo; if that does not work, also grant the installed Nova
binary. On `permission_denied`, fix the Accessibility grant before using
fallbacks.
