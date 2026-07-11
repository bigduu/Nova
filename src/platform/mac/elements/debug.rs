//! Developer diagnostics behind the `--dump-ax` / `--hit-dump` / `--ax-warm` CLI
//! flags. None of this runs in the normal MCP path; it exists to SEE why some web
//! content is or isn't marked (does it expose an `AXWebArea`? real actions? what
//! roles do the rows have? does the tree warm up?).

use super::attrs::{
    ax_actions, ax_label, ax_pair, ax_role, ax_subrole, click_action_for, element_rect, Rect,
};
use super::hittest::{actionable_self_or_ancestor, element_at_position, hit_test_elements};
use super::walk::{child_elements, MAX_DEPTH};
use super::warmth::enable_web_accessibility;
use accessibility::AXUIElement;
use accessibility_sys::{kAXValueTypeCGPoint, kAXValueTypeCGSize};
use std::time::Duration;

/// Dump the AX tree of `pid` (from the app element) as indented text — role,
/// subrole, label, supported actions, and frame. Uses the SAME `child_elements`
/// traversal as marks, so it shows exactly what the marks walk reaches.
pub fn dump_tree(pid: i32, max_nodes: usize) -> String {
    let app = AXUIElement::application(pid);
    enable_web_accessibility(&app);
    std::thread::sleep(Duration::from_millis(600)); // let the web tree build
    let mut out = String::new();
    let mut n = 0usize;
    dump_node(&app, 0, &mut out, &mut n, max_nodes);
    out.push_str(&format!(
        "\n[{n} nodes dumped{}]\n",
        if n >= max_nodes { ", TRUNCATED" } else { "" }
    ));
    out
}

fn dump_node(el: &AXUIElement, depth: usize, out: &mut String, n: &mut usize, max: usize) {
    if *n >= max || depth >= MAX_DEPTH {
        return;
    }
    *n += 1;
    let role = ax_role(el);
    let subrole = match ax_subrole(el) {
        s if s.is_empty() => String::new(),
        s => format!("[{s}]"),
    };
    let label = ax_label(el);
    let lbl = if label.is_empty() {
        String::new()
    } else {
        let t: String = label.chars().take(40).collect();
        format!(" {t:?}")
    };
    let actions = ax_actions(el);
    let act = if actions.is_empty() {
        String::new()
    } else {
        format!(" actions={actions:?}")
    };
    let frm = ax_pair(el, "AXPosition", kAXValueTypeCGPoint)
        .zip(ax_pair(el, "AXSize", kAXValueTypeCGSize))
        .map(|((x, y), (w, h))| format!(" @({x:.0},{y:.0} {w:.0}x{h:.0})"))
        .unwrap_or_default();
    let indent = "  ".repeat(depth.min(40));
    out.push_str(&format!("{indent}{role}{subrole}{lbl}{act}{frm}\n"));
    for child in child_elements(el, &role) {
        if *n >= max {
            break;
        }
        dump_node(&child, depth + 1, out, n, max);
    }
}

/// A compact `role:actions` breadcrumb of `el` and up to `depth` ancestors, e.g.
/// `StaticText:["ShowMenu"] < Group:["ShowMenu"] < ...`. Lets a hit-test
/// diagnostic show whether a real control hides up-tree.
fn ancestor_chain(el: &AXUIElement, depth: usize) -> String {
    use accessibility::AXAttribute;
    let mut parts = Vec::new();
    let mut cur = el.clone();
    for _ in 0..=depth {
        let role = ax_role(&cur).replace("AX", "");
        let acts: Vec<String> = ax_actions(&cur)
            .into_iter()
            .map(|s| s.replace("AX", "").replace('\n', " "))
            .collect();
        parts.push(format!("{role}:{acts:?}"));
        match cur.attribute(&AXAttribute::parent()).ok() {
            Some(p) => cur = p,
            None => break,
        }
    }
    parts.join(" < ")
}

/// Hit-test a grid over `clip` and, for every DISTINCT element the window server
/// reports, print what it is and whether our actionable-ancestor climb accepts
/// it — plus the full ancestor chain, so a row the eye sees but marks miss shows
/// up as either no hit, or a hit whose climb yields NONE (partial vs full
/// Chromium AX mode). `inset` skips the left native sidebar.
pub fn hit_dump(pid: i32, clip: Rect, step: f64, inset: f64) -> String {
    let app = AXUIElement::application(pid);
    enable_web_accessibility(&app);
    std::thread::sleep(Duration::from_millis(600)); // let the web tree build

    let (cx, cy, cw, ch) = clip;
    let x0 = cx + inset;
    let system_wide = AXUIElement::system_wide();
    let mut out = String::new();
    let mut seen: std::collections::HashSet<(i64, i64, i64, i64)> =
        std::collections::HashSet::new();
    let mut hits = 0usize;
    let mut accepted = 0usize;
    let mut no_hit = 0usize;

    let mut y = cy + step / 2.0;
    while y < cy + ch {
        let mut x = x0 + step / 2.0;
        while x < cx + cw {
            match element_at_position(&system_wide, x, y) {
                None => no_hit += 1,
                Some(hit) => {
                    let hr = element_rect(&hit).unwrap_or((0.0, 0.0, 0.0, 0.0));
                    let key = (hr.0 as i64, hr.1 as i64, hr.2 as i64, hr.3 as i64);
                    if seen.insert(key) {
                        hits += 1;
                        let role = ax_role(&hit);
                        let subrole = match ax_subrole(&hit) {
                            s if s.is_empty() => String::new(),
                            s => format!("[{s}]"),
                        };
                        let label: String = ax_label(&hit).chars().take(40).collect();
                        let actions = ax_actions(&hit);
                        let chain = ancestor_chain(&hit, 8);
                        let verdict = match actionable_self_or_ancestor(&hit) {
                            Some(target) => {
                                accepted += 1;
                                let tr = ax_role(&target);
                                let tlbl: String = ax_label(&target).chars().take(30).collect();
                                let ta = click_action_for(&target).unwrap_or("(role-only)");
                                format!("ACCEPT -> {tr} {tlbl:?} via {ta}")
                            }
                            None => format!("REJECT  chain=[{chain}]"),
                        };
                        out.push_str(&format!(
                            "@({x:.0},{y:.0}) hit {role}{subrole} {label:?} actions={actions:?} @({:.0},{:.0} {:.0}x{:.0}) => {verdict}\n",
                            hr.0, hr.1, hr.2, hr.3
                        ));
                    }
                }
            }
            x += step;
        }
        y += step;
    }
    out.push_str(&format!(
        "\n[{hits} distinct hits, {accepted} accepted, {} rejected, {no_hit} empty samples; content region x>={x0:.0}]\n",
        hits - accepted
    ));
    out
}

/// In ONE long-lived process, set the web-AX enable once and then probe the
/// content area repeatedly, printing how many genuine controls the hit-test finds
/// each round — tests whether a long-lived process stabilizes Chromium's full
/// semantic tree (the "Homerow way") vs the cold one-shot CLI that flickers.
pub fn ax_warm_probe(pid: i32, clip: Rect, rounds: usize) -> String {
    let app = AXUIElement::application(pid);
    let accepted0 = enable_web_accessibility(&app);
    let mut out = String::new();
    out.push_str(&format!(
        "AXManualAccessibility accepted={accepted0}; probing {rounds} rounds (~500ms apart)\n"
    ));
    let (cx, cy, cw, ch) = clip;
    let x0 = cx + 280.0; // skip the left native sidebar
    for round in 0..rounds {
        enable_web_accessibility(&app); // mirror a server keeping it warm
        let found = hit_test_elements((x0, cy, cw - 280.0, ch), 30.0, &[]);
        let total = found.len();
        let with_action = found
            .iter()
            .filter(|(_, h)| click_action_for(h.element()).is_some())
            .count();
        let roles: std::collections::BTreeMap<String, usize> =
            found
                .iter()
                .fold(std::collections::BTreeMap::new(), |mut m, (e, _)| {
                    *m.entry(e.role.clone()).or_default() += 1;
                    m
                });
        out.push_str(&format!(
            "round {round}: {total} actionable ({with_action} with AXPress-like)  roles={roles:?}\n"
        ));
        std::thread::sleep(Duration::from_millis(500));
    }
    out
}
