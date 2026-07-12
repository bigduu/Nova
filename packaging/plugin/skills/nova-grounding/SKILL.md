---
name: nova-grounding
description: Coordinate/marks/click_mark grounding workflow for driving Nova's macOS desktop-automation MCP tools (screenshot, click_mark, zoom_region, ocr, and the input tools). Use whenever a task requires controlling the macOS desktop through Nova to reliably target the right pixel or UI element instead of guessing coordinates.
---

# Nova desktop grounding

Nova controls the macOS desktop: screenshots plus mouse/keyboard. All
click/move/scroll coordinates are in the pixel space of the MOST RECENT
screenshot.

## Targeting an element, in priority order

Do not jump to raw coordinates first.

1. **Best: click by mark number.** A window/display `screenshot` numbers
   every actionable element BY DEFAULT (`marks` is on; needs Accessibility
   permission); each is listed as `[N] role "label"`. Call
   `click_mark(number=N)` to activate `[N]`. This activates the control in
   the background with no cursor movement — web-page content via the page's
   own JavaScript engine (an Accessibility press is a no-op on web content),
   native controls via the Accessibility tree — falling back to a coordinate
   click only if neither applies. It is the most reliable path: use it
   whenever the element you want appears in the marks list. The numbers
   reset on every marks capture and go stale when the UI changes, so take a
   fresh `screenshot(marks=true)` right before each `click_mark`.
2. **Let the app find it for you.** Prefer the app's own search (click the
   search box, type the name, press Enter) over visually scanning a long
   list — far more reliable than estimating a row's position.
3. **Fallback: read coordinates**, only when the target is NOT in the marks
   list. Web pages are covered by marks too: real links/buttons on semantic
   pages, and on div-rendered pages (e.g. webmail) the list ROWS are numbered
   as well (clicking such a row lands via a coordinate at its center, so it
   still needs a fresh `marks=true` shot right before, since these go stale
   on scroll). So coordinate mode is mainly for canvas / game / custom-
   rendered surfaces that expose no marks at all. Then:
   - Do NOT guess off a full-display `screenshot`: it is downscaled (max
     ~1280px wide), so on a busy/Retina screen small UI is only a few pixels
     tall — too small to read or click. Capture the specific window:
     `screenshot(window="<name>")` (larger, sharper, clicks map into the
     window automatically), and if the target is still small, zoom:
     `zoom_region(x, y, width, height)` re-captures that rectangle (in the
     last shot's pixel space) at native resolution. Click only once the
     target is clearly legible.
   - In this coordinate mode a labeled magenta grid is overlaid
     automatically (rules with their pixel x/y values along the edges): read
     a target's (x, y) off the nearest labeled rules and interpolate within
     the cell instead of guessing. (The grid is shown whenever `marks` is
     off; with `marks` on it is hidden since you click by number — pass
     `grid=true` if you want both.)
   - To READ or click text on such a marks-less surface, prefer `ocr` over
     eyeballing the grid — see "Reading text" below.

## Confirm every action — do not operate blind

- After EACH input action (click, scroll, type, key press) take a
  screenshot to see the result BEFORE deciding the next action. Never fire
  several scrolls or clicks in a row without a screenshot in between — you
  cannot read what you scrolled past, and an unconfirmed click may have
  missed.
- When reading a long view by scrolling, scroll ONE step, screenshot, read,
  then scroll again — capturing each screen so nothing is skipped.

## Keep captures focused

Once you know WHICH part of the screen matters, capture just that part
instead of the whole display: `screenshot(window="<name>")` or
`zoom_region(x, y, width, height)` returns a smaller, sharper image with
fewer pixels to read (faster turnaround, less context). Reserve the
full-display capture for orienting or finding which window to target; for
repeated work inside one app or panel, stay scoped to it.

## Targeting a window by name

- The `window="<name>"` argument is a case-insensitive SUBSTRING of an
  on-screen window's title or its app's name, exactly as it appears on
  screen — match the literal on-screen text, do not translate or
  transliterate it.
- If your guess is wrong the tool does NOT guess for you: it returns
  "no on-screen window matching …" and LISTS the windows that are actually
  on screen. Read that list and retry with the correct name — do not repeat
  the same guess.
- When you do not already know the exact on-screen name, take a
  full-display `screenshot` first (omit `window=`) to read the real
  window/app names, then target one.

## Reading text — when to use `ocr`

- Use `ocr` to (a) read a lot of text at once (a chat thread, an article, a
  log, a list/table) — it returns the lines as text, far cheaper than
  parsing a screenshot image; or (b) read or click text on a surface where
  `marks` comes back empty or sparse (canvas, games, image-/custom-rendered
  views, chat bubbles). Each line carries a clickable center, so
  `left_click(x, y)` a line to click text that is not an Accessibility
  element.
- Do NOT reach for `ocr` when the target IS an actionable native/web control
  (button, link, field, list row): `screenshot(marks=true)` + `click_mark`
  is more precise. And `ocr` returns no image, so when you need to SEE
  layout / icons / state, take a `screenshot`.
- Combine by role within one window: the native chrome (sidebar, toolbar,
  buttons) is usually marked — use marks + `click_mark` there; the content
  (message bubbles, a rendered document) is often AX-less — use `ocr` to
  read or click it. Typical flow: `screenshot(window="X", marks=true)` to
  act on controls, then `ocr(window="X")` to read the content.

## Typing

`type_text` accepts ANY text, including non-ASCII (e.g. 中文) and emoji. To
enter something by name, click the field and type it directly.

## Foreground vs background input

- By default clicks/scroll/typing go to the foreground (the real cursor
  moves; the target window is activated). This works for every app,
  including browsers and Electron apps.
- For a native macOS app you do not want to disturb, pass `background=true`
  on a click/scroll/type to deliver it straight to the captured window's
  process without moving the cursor or raising the window. It only works
  after a `window=` capture, and browsers / Electron / custom-rendered apps
  ignore it — retry without `background` if it has no visible effect.
- `click_mark(number=N)` (preferred) or `ax_click`/`ax_set_value`/`ax_focus`
  (label-match by role/text substring) drive controls directly through the
  Accessibility tree, in the background, with no coordinates. The
  label-match tools return "no element" on div-rendered pages (use
  `click_mark` on a row mark there instead) and on canvas/game surfaces with
  no tree.

## Typical workflow

For "find X inside app Y": `screenshot(window="Y", marks=true)` → if X is
listed, `click_mark(number=N)` → screenshot to confirm. If X is not in the
marks, use Y's search box or `zoom_region` until X is legible, then click
its coordinates → screenshot to confirm.

## Permissions

Nova needs macOS Screen Recording (for `screenshot`/`ocr`/`list_windows`)
and Accessibility (for posting input and Set-of-Mark) permissions. As an
unbundled subprocess these prompts and grants attach to the LAUNCHING
PROCESS — i.e. Bamboo, not this plugin's `nova` binary. If a tool call fails
with a permission error, grant Bamboo access under System Settings →
Privacy & Security → Screen Recording / Accessibility and retry.
