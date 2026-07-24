//! Background clicking of WEB content by driving the browser's own JavaScript
//! engine via AppleScript (`osascript`), instead of an Accessibility action.
//!
//! Why this exists: on Chromium/WebKit page content, `AXUIElementPerformAction
//! (AXPress)` returns success but is a NO-OP (the page never navigates), and a
//! pid-targeted `CGEvent` is ignored by the browser's own event handling. The
//! only background-capable path (no cursor movement, app need not be frontmost)
//! that actually fires the page's handlers is running
//! `document.elementFromPoint(x, y).click()` inside the page. Browsers expose
//! that through AppleScript: the Chromium family via `execute … javascript`, and
//! Safari via `do JavaScript … in current tab`.
//!
//! This applies ONLY to elements under an `AXWebArea` in a scriptable browser.
//! Native chrome (a toolbar/tab even in Safari or Chrome) is not under a web
//! area and keeps the reliable AX-action path. Electron apps (Slack, VS Code,
//! Tauri shells) expose no JS-exec AppleScript command, so they are not matched
//! here and fall back to AX / coordinate clicking.

use std::process::{Command, Stdio};

/// A scriptable browser we can drive via AppleScript JS execution.
pub(crate) struct Browser {
    /// AppleScript application name (e.g. "Safari", "Google Chrome", "Arc").
    name: &'static str,
    /// WebKit (Safari) uses `do JavaScript … in current tab of front window`;
    /// the Chromium family uses `execute active tab of front window javascript`.
    webkit: bool,
}

impl Browser {
    pub(crate) fn name(&self) -> &'static str {
        self.name
    }
}

/// Identify the scriptable browser owning `pid` from its bundle's `.app` name
/// (taken from the executable path — no extra frameworks). Returns `None` for
/// anything we cannot drive via JS-exec AppleScript (non-browsers AND Electron
/// browsers-in-disguise, which expose no such command).
pub(crate) fn browser_for_pid(pid: i32) -> Option<Browser> {
    let exe = crate::platform::mac::geometry::proc_path(pid)?;
    // …/Arc.app/Contents/MacOS/Arc → the ".app" path component is the bundle.
    let app = exe.split('/').rfind(|c| c.ends_with(".app"))?;
    let (name, webkit) = match app {
        "Safari.app" | "Safari Technology Preview.app" => ("Safari", true),
        "Arc.app" => ("Arc", false),
        "Google Chrome.app" => ("Google Chrome", false),
        "Google Chrome Canary.app" => ("Google Chrome Canary", false),
        "Brave Browser.app" => ("Brave Browser", false),
        "Microsoft Edge.app" => ("Microsoft Edge", false),
        "Vivaldi.app" => ("Vivaldi", false),
        "Chromium.app" => ("Chromium", false),
        _ => return None,
    };
    Some(Browser { name, webkit })
}

/// Click the element matching `label` nearest the in-page point `(cx, cy)` of the
/// browser's active tab, falling back to a hit-test at the point. `label` is the
/// element's accessibility name (so we don't depend on pixel-exact coordinates,
/// which break under page zoom / dpr / scroll — the point only DISAMBIGUATES
/// among same-named elements). Returns the clicked tag on success, or an error
/// the caller can fall back on (osascript missing, Automation/JS-from-Apple-Events
/// not permitted, nothing found, …).
///
/// NB: targets the FRONT window's active tab. For the common single-window /
/// frontmost-window case this is the captured page; a background non-front tab
/// is not addressed here (the caller's AX/coordinate fallback still applies).
pub(crate) fn js_click_at(
    browser: &Browser,
    cx: f64,
    cy: f64,
    label: &str,
    deadline: std::time::Instant,
) -> Result<String, String> {
    let (cx, cy) = (cx.round() as i64, cy.round() as i64);
    let label_lit = js_string(label);
    // Page-context: prefer the visible clickable whose accessible name matches the
    // label, nearest the point; else hit-test the point and climb to a clickable.
    // Single-quoted selector + backslash regex are escaped for AppleScript below.
    let js = format!(
        "(function(){{\
         var L={label_lit},ax={cx},ay={cy};\
         var SEL='a,button,[role=button],[role=link],[role=tab],[role=menuitem],[role=option],[role=checkbox],summary,input,label,select';\
         function vis(e){{var r=e.getBoundingClientRect();return r.width>0&&r.height>0;}}\
         function norm(s){{return (s||'').replace(/\\s+/g,' ').trim();}}\
         var key=norm(L).replace(/\\(.*$/,'').replace(/[^A-Za-z0-9 ]/g,'').trim().toLowerCase();\
         var pick=null;\
         if(key.length>=2){{\
           var c=Array.prototype.slice.call(document.querySelectorAll(SEL)).filter(function(e){{if(!vis(e))return false;var t=norm(e.getAttribute('aria-label')||e.textContent).toLowerCase();return t&&t.indexOf(key)>=0;}});\
           if(c.length){{c.sort(function(a,b){{var ra=a.getBoundingClientRect(),rb=b.getBoundingClientRect();return Math.hypot(ra.left+ra.width/2-ax,ra.top+ra.height/2-ay)-Math.hypot(rb.left+rb.width/2-ax,rb.top+rb.height/2-ay);}});pick=c[0];}}\
         }}\
         if(!pick){{var h=document.elementFromPoint(ax,ay);pick=h?(h.closest(SEL)||h):null;}}\
         if(!pick){{return 'NO_TARGET';}}\
         pick.click();return 'OK '+pick.tagName+' \"'+norm(pick.getAttribute('aria-label')||pick.textContent).slice(0,24)+'\"';}})()"
    );
    let script = if browser.webkit {
        format!(
            "tell application \"{}\" to do JavaScript \"{}\" in current tab of front window",
            browser.name,
            escape_applescript(&js)
        )
    } else {
        format!(
            "tell application \"{}\" to execute active tab of front window javascript \"{}\"",
            browser.name,
            escape_applescript(&js)
        )
    };

    let mut child = Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("osascript spawn failed: {e}"))?;
    let out = loop {
        match child.try_wait() {
            Ok(Some(_)) => break child.wait_with_output().map_err(|e| e.to_string())?,
            Ok(None) if std::time::Instant::now() < deadline => {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                std::thread::sleep(std::cmp::min(
                    std::time::Duration::from_millis(20),
                    remaining,
                ));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("osascript web click exceeded the action deadline".to_string());
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("osascript status check failed: {error}"));
            }
        }
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let result = stdout.trim().trim_matches('"').trim();
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("osascript error: {}", err.trim()));
    }
    if result.starts_with("OK") {
        Ok(result.to_string())
    } else {
        // NO_ELEMENT, empty (JS-from-Apple-Events disabled in Safari), etc.
        Err(format!("page did not click ({result})"))
    }
}

/// Escape a string for embedding inside an AppleScript double-quoted literal.
/// (Applied LAST, so any backslashes the JS needs — regex `\s`, the JS-string
/// escapes from [`js_string`] — get doubled for AppleScript and arrive intact.)
fn escape_applescript(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Render `s` as a JS single-quoted string literal (for the `label`). Newlines
/// are flattened to spaces so the one-line AppleScript stays one line.
fn js_string(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' | '\r' => out.push(' '),
            _ => out.push(c),
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applescript_escaping() {
        assert_eq!(escape_applescript("a'b"), "a'b"); // single quotes untouched
        assert_eq!(escape_applescript("a\"b"), "a\\\"b");
        assert_eq!(escape_applescript("a\\b"), "a\\\\b");
    }

    #[test]
    fn js_string_literal() {
        assert_eq!(js_string("Issues (13)"), "'Issues (13)'");
        assert_eq!(js_string("a'b"), "'a\\'b'");
        assert_eq!(js_string("a\\b"), "'a\\\\b'");
        assert_eq!(js_string("a\nb"), "'a b'");
    }
}
