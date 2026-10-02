//! Nova.app's coordinate-input feedback. AppKit stays on its existing main
//! thread; input workers replace one latest cue and never wait for the UI.

use objc2::rc::Retained;
use objc2::{define_class, msg_send, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSBezierPath, NSColor, NSCompositingOperation, NSPanel,
    NSRectFillUsingOperation, NSScreen, NSStatusWindowLevel, NSView, NSWindowCollectionBehavior,
    NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};
use std::cell::Cell;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Also used by the separate capture daemon, with executable-owner validation.
pub(crate) const WINDOW_TITLE: &str = "Nova Native Virtual Cursor";
const IDLE: Duration = Duration::from_millis(1200);
const FEEDBACK: Duration = Duration::from_millis(400);
const SIZE: (f64, f64) = (52.0, 60.0);
const HOTSPOT: (f64, f64) = (16.0, 42.0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CueKind {
    Move,
    LeftClick,
    RightClick,
    DoubleClick,
    Scroll(i32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Drawing {
    point: (f64, f64),
    kind: CueKind,
}

#[derive(Clone, Copy)]
struct Cue {
    drawing: Drawing,
    at: Instant,
}

#[derive(Default)]
struct CueState {
    enabled: bool,
    latest: Option<Cue>,
}

impl CueState {
    fn start(&mut self) {
        self.enabled = true;
        self.latest = None;
    }

    fn stop(&mut self) {
        self.enabled = false;
        self.latest = None;
    }

    fn publish(&mut self, point: (f64, f64), kind: CueKind, at: Instant) {
        if self.enabled && !SUPPRESSED.get() && point.0.is_finite() && point.1.is_finite() {
            self.latest = Some(Cue {
                drawing: Drawing { point, kind },
                at,
            });
        }
    }

    fn drawing(&mut self, now: Instant) -> Option<Drawing> {
        let cue = self.latest?;
        let age = now.saturating_duration_since(cue.at);
        if age >= IDLE {
            self.latest = None;
            return None;
        }
        Some(Drawing {
            kind: if age >= FEEDBACK {
                CueKind::Move
            } else {
                cue.drawing.kind
            },
            ..cue.drawing
        })
    }
}

fn mailbox() -> &'static Mutex<CueState> {
    static MAILBOX: OnceLock<Mutex<CueState>> = OnceLock::new();
    MAILBOX.get_or_init(Mutex::default)
}

thread_local! {
    static SUPPRESSED: Cell<bool> = const { Cell::new(false) };
}

/// Restore a user's real pointer without making that internal move a cue.
/// Only this worker is muted; nested scopes and unwinding restore its state.
pub(crate) fn without_feedback<T>(action: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            SUPPRESSED.set(self.0);
        }
    }
    let _restore = Restore(SUPPRESSED.replace(true));
    action()
}

pub(crate) fn feedback(x: f64, y: f64, kind: CueKind) {
    if let Ok(mut state) = mailbox().lock() {
        state.publish((x, y), kind, Instant::now());
    }
}

/// CoreGraphics has a top-left origin; AppKit uses the primary screen's
/// bottom-left origin. Both are logical points, including secondary displays.
fn panel_origin(point: (f64, f64), primary_top: f64) -> Option<(f64, f64)> {
    let origin = (point.0 - HOTSPOT.0, primary_top - point.1 - HOTSPOT.1);
    (primary_top > 0.0 && origin.0.is_finite() && origin.1.is_finite()).then_some(origin)
}

define_class!(
    // SAFETY: no additional NSPanel subclass requirements. Instances and
    // all AppKit calls remain on the existing Nova.app main thread.
    #[unsafe(super = NSPanel)]
    #[thread_kind = MainThreadOnly]
    struct CursorPanel;

    unsafe impl NSObjectProtocol for CursorPanel {}

    impl CursorPanel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool { false }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main_window(&self) -> bool { false }
    }
);

define_class!(
    // SAFETY: NSView's designated initializer is called below. Its drawing
    // ivar and callbacks are used exclusively on the main thread.
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = Cell<Option<Drawing>>]
    struct CursorView;

    unsafe impl NSObjectProtocol for CursorView {}

    impl CursorView {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool { false }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, rect: NSRect) {
            // Clear the previous arrow/ring even when the new drawing is empty.
            NSColor::clearColor().set();
            NSRectFillUsingOperation(rect, NSCompositingOperation::Copy);
            if let Some(drawing) = self.ivars().get() {
                draw_cursor(drawing.kind);
            }
        }
    }
);

fn path(points: &[(f64, f64)]) -> Retained<NSBezierPath> {
    let path = NSBezierPath::bezierPath();
    path.moveToPoint(NSPoint::new(points[0].0, points[0].1));
    for &(x, y) in &points[1..] {
        path.lineToPoint(NSPoint::new(x, y));
    }
    path
}

fn accent() -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(0.64, 0.22, 1.0, 1.0)
}

fn ring(radius: f64) {
    let ring = NSBezierPath::bezierPathWithOvalInRect(NSRect::new(
        NSPoint::new(HOTSPOT.0 - radius, HOTSPOT.1 - radius),
        NSSize::new(radius * 2.0, radius * 2.0),
    ));
    NSColor::whiteColor().set();
    ring.setLineWidth(4.0);
    ring.stroke();
    accent().set();
    ring.setLineWidth(2.0);
    ring.stroke();
}

fn draw_cursor(kind: CueKind) {
    match kind {
        CueKind::LeftClick => ring(10.0),
        CueKind::RightClick => ring(14.0),
        CueKind::DoubleClick => {
            ring(10.0);
            ring(14.0);
        }
        CueKind::Scroll(lines) if lines != 0 => {
            let sign = lines.signum() as f64;
            let tip = 38.0 + sign * 9.0;
            let arrow = path(&[(42.0, 38.0 - sign * 9.0), (42.0, tip)]);
            arrow.moveToPoint(NSPoint::new(35.0, tip - sign * 7.0));
            arrow.lineToPoint(NSPoint::new(42.0, tip));
            arrow.lineToPoint(NSPoint::new(49.0, tip - sign * 7.0));
            NSColor::whiteColor().set();
            arrow.setLineWidth(5.0);
            arrow.stroke();
            accent().set();
            arrow.setLineWidth(3.0);
            arrow.stroke();
        }
        _ => {}
    }
    let arrow = path(&[
        HOTSPOT,
        (16.0, 18.0),
        (22.0, 24.0),
        (28.0, 12.0),
        (33.0, 15.0),
        (27.0, 27.0),
        (35.0, 27.0),
    ]);
    arrow.closePath();
    NSColor::blackColor().set();
    arrow.setLineWidth(4.0);
    arrow.stroke();
    NSColor::whiteColor().set();
    arrow.setLineWidth(2.0);
    arrow.stroke();
    accent().set();
    arrow.fill();
}

/// Owned by status_menu::run, alongside its menu and listener lifetime.
pub(crate) struct CursorOverlay {
    panel: Retained<CursorPanel>,
    view: Retained<CursorView>,
}

impl CursorOverlay {
    pub(crate) fn new(mtm: MainThreadMarker) -> Self {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(SIZE.0, SIZE.1));
        // SAFETY: the documented NSPanel initializer and its arguments match.
        // Allocate its WindowServer identity before starting the MCP listener;
        // keep the transparent, empty window ordered so capture can find it
        // before the first cue. No permission or activation API is invoked.
        let panel: Retained<CursorPanel> = unsafe {
            msg_send![CursorPanel::alloc(mtm),
                initWithContentRect: frame,
                styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
                backing: NSBackingStoreType::Buffered,
                defer: false]
        };
        let allocated = CursorView::alloc(mtm).set_ivars(Cell::new(None));
        // SAFETY: initialize NSView's superclass with its designated initializer.
        let view: Retained<CursorView> =
            unsafe { msg_send![super(allocated), initWithFrame: frame] };
        panel.setTitle(&NSString::from_str(WINDOW_TITLE));
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHasShadow(false);
        panel.setIgnoresMouseEvents(true);
        panel.setHidesOnDeactivate(false);
        panel.setFloatingPanel(true);
        panel.setLevel(NSStatusWindowLevel);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        // SAFETY: Rust retains the panel until after explicit close in Drop.
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setContentView(Some(&view));
        panel.orderFrontRegardless();
        if let Ok(mut state) = mailbox().lock() {
            state.start();
        }
        Self { panel, view }
    }

    pub(crate) fn tick(&self) {
        let mut drawing = mailbox()
            .lock()
            .ok()
            .and_then(|mut s| s.drawing(Instant::now()));
        if let Some(cue) = drawing {
            // mainScreen follows keyboard focus, whereas screens[0] is the
            // primary origin. Re-read it because display layouts can change.
            let origin = NSScreen::screens(self.view.mtm())
                .firstObject()
                .and_then(|screen| {
                    let frame = screen.frame();
                    panel_origin(cue.point, frame.origin.y + frame.size.height)
                });
            if let Some((x, y)) = origin {
                self.panel.setFrameOrigin(NSPoint::new(x, y));
            } else {
                drawing = None;
            }
        }
        if self.view.ivars().replace(drawing) != drawing {
            self.view.setNeedsDisplay(true);
        }
    }

    pub(crate) fn stop(&self) {
        if let Ok(mut state) = mailbox().lock() {
            state.stop();
        }
        self.view.ivars().set(None);
        self.view.setNeedsDisplay(true);
        self.panel.displayIfNeeded();
    }
}

impl Drop for CursorOverlay {
    fn drop(&mut self) {
        self.stop();
        self.panel.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_cue_is_bounded_and_disabled_without_app_owner() {
        let now = Instant::now();
        let mut state = CueState::default();
        state.publish((1.0, 2.0), CueKind::Move, now);
        assert!(state.drawing(now).is_none());
        state.start();
        for index in 0..10_000 {
            state.publish((index as f64, -20.0), CueKind::Move, now);
        }
        assert_eq!(state.drawing(now).unwrap().point, (9999.0, -20.0));
        state.publish((f64::NAN, 0.0), CueKind::LeftClick, now);
        state.publish((0.0, f64::INFINITY), CueKind::Move, now);
        assert_eq!(state.drawing(now).unwrap().point, (9999.0, -20.0));
    }

    #[test]
    fn ring_and_scroll_expire_before_arrow_and_idle_clears() {
        let now = Instant::now();
        let mut state = CueState::default();
        state.start();
        for kind in [
            CueKind::LeftClick,
            CueKind::RightClick,
            CueKind::DoubleClick,
            CueKind::Scroll(-3),
        ] {
            state.publish((25.0, 40.0), kind, now);
            assert_eq!(state.drawing(now + FEEDBACK / 2).unwrap().kind, kind);
            assert_eq!(state.drawing(now + FEEDBACK).unwrap().kind, CueKind::Move);
            assert!(state.drawing(now + IDLE).is_none());
            assert!(state.latest.is_none());
        }
    }

    #[test]
    fn shutdown_clears_and_late_worker_cannot_restore_cue() {
        let now = Instant::now();
        let mut state = CueState::default();
        state.start();
        state.publish((10.0, 20.0), CueKind::DoubleClick, now);
        state.stop();
        state.publish((30.0, 40.0), CueKind::Scroll(2), now);
        assert!(state.drawing(now).is_none());
        state.start();
        assert!(state.drawing(now).is_none());
    }

    #[test]
    fn hotspot_preserves_logical_coordinates_and_negative_origins() {
        let primary_top = 900.0;
        for point in [
            (0.0, 0.0),
            (720.0, 450.0),
            (-400.0, 300.0),
            (1500.0, -200.0),
        ] {
            let origin = panel_origin(point, primary_top).unwrap();
            assert_eq!(origin.0 + HOTSPOT.0, point.0);
            assert_eq!(primary_top - (origin.1 + HOTSPOT.1), point.1);
        }
        // Retinal/mixed-scale captures convert to logical points BEFORE input.
        for scale in [1.0, 2.0] {
            let view = crate::display::view::ViewFrame {
                origin: (-500.0, 200.0),
                region: (800.0, 600.0),
                screenshot: (800.0 * scale, 600.0 * scale),
            };
            let point = view.to_logical(400.0 * scale, 300.0 * scale);
            assert_eq!(point, (-100.0, 500.0));
            let origin = panel_origin(point, primary_top).unwrap();
            assert_eq!(
                (origin.0 + HOTSPOT.0, origin.1 + HOTSPOT.1),
                (-100.0, 400.0)
            );
        }
        assert!(panel_origin((0.0, 0.0), 0.0).is_none());
        assert!(panel_origin((0.0, f64::NAN), 900.0).is_none());
    }

    #[test]
    fn pointer_restoration_is_nested_worker_local_and_unwind_safe() {
        let now = Instant::now();
        let mut state = CueState::default();
        state.start();
        state.publish((30.0, 40.0), CueKind::LeftClick, now);
        without_feedback(|| {
            // Simulates the normal production mouse-move publisher on restore.
            state.publish((1.0, 2.0), CueKind::Move, now);
            without_feedback(|| state.publish((3.0, 4.0), CueKind::Move, now));
            state.publish((5.0, 6.0), CueKind::Move, now);
            assert_eq!(state.drawing(now).unwrap().point, (30.0, 40.0));
            std::thread::spawn(move || {
                let mut other = CueState::default();
                other.start();
                other.publish((7.0, 8.0), CueKind::RightClick, now);
                assert_eq!(other.drawing(now).unwrap().point, (7.0, 8.0));
            })
            .join()
            .unwrap();
        });
        let panic =
            std::panic::catch_unwind(|| without_feedback(|| panic!("fixture restore failed")));
        assert!(panic.is_err());
        state.publish((9.0, 10.0), CueKind::Move, now);
        assert_eq!(state.drawing(now).unwrap().point, (9.0, 10.0));
    }
}
