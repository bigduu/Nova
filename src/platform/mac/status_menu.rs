//! Native status menu for the independently launched Nova.app. UI stays on
//! the main thread; the MCP service and its pairing queue remain independent.

use super::permissions::SystemPermissions;
use crate::app_status::{
    AppStatus, Controller, Permission, PermissionState, ServiceState, Snapshot,
};
use anyhow::{Context, Result};
use objc2::rc::Retained;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem,
    NSVariableStatusItemLength,
};
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSString};
use std::cell::{Cell, RefCell};

struct MenuIvars {
    status: AppStatus,
    controller: Controller<SystemPermissions>,
    service: Retained<NSMenuItem>,
    accessibility: Retained<NSMenuItem>,
    screen_recording: Retained<NSMenuItem>,
    request_accessibility: Retained<NSMenuItem>,
    request_screen: Retained<NSMenuItem>,
    notice: Retained<NSMenuItem>,
    item: Retained<NSStatusItem>,
    previous: RefCell<Option<Snapshot>>,
    quit: Cell<bool>,
}

define_class!(
    // SAFETY: NSObject has no subclass requirements. All menu references and
    // callbacks remain on the process main thread for their full lifetime.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = MenuIvars]
    struct MenuTarget;

    unsafe impl NSObjectProtocol for MenuTarget {}

    impl MenuTarget {
        #[unsafe(method(novaMenuAction:))]
        fn menu_action(&self, sender: &NSMenuItem) {
            let ivars = self.ivars();
            match sender.tag() {
                1 => ivars.controller.refresh(),
                2 => ivars.controller.request(Permission::Accessibility),
                3 => ivars.controller.open_settings(Permission::Accessibility),
                4 => ivars.controller.request(Permission::ScreenRecording),
                5 => ivars.controller.open_settings(Permission::ScreenRecording),
                6 => ivars.quit.set(true),
                _ => return,
            }
            self.render();
        }
    }
);

impl MenuTarget {
    fn render(&self) {
        let ivars = self.ivars();
        let snapshot = ivars.status.snapshot();
        if ivars.previous.borrow().as_ref() == Some(&snapshot) {
            return;
        }
        let service = match snapshot.service {
            ServiceState::Starting => "Service: Starting…",
            ServiceState::Ready => "Service: Ready",
            ServiceState::Failed => "Service: Failed — quit and open Nova again",
        };
        ivars.service.setTitle(&NSString::from_str(service));
        let permission = |state| match state {
            PermissionState::NotChecked => "Checking…",
            PermissionState::Granted => "Granted",
            PermissionState::NotGranted => "Not granted",
        };
        ivars.accessibility.setTitle(&NSString::from_str(&format!(
            "Accessibility: {}",
            permission(snapshot.accessibility)
        )));
        ivars
            .screen_recording
            .setTitle(&NSString::from_str(&format!(
                "Screen Recording: {}",
                permission(snapshot.screen_recording)
            )));
        ivars
            .request_accessibility
            .setEnabled(snapshot.accessibility != PermissionState::Granted);
        ivars
            .request_screen
            .setEnabled(snapshot.screen_recording != PermissionState::Granted);
        ivars.notice.setHidden(snapshot.notice.is_none());
        ivars.notice.setTitle(&NSString::from_str(
            snapshot.notice.as_deref().unwrap_or(""),
        ));
        if let Some(button) = ivars.item.button(self.mtm()) {
            button.setTitle(&NSString::from_str(match snapshot.service {
                ServiceState::Starting => "Nova …",
                ServiceState::Ready => "Nova",
                ServiceState::Failed => "Nova !",
            }));
        }
        *ivars.previous.borrow_mut() = Some(snapshot);
    }
}

struct StatusMenu {
    item: Retained<NSStatusItem>,
    target: Retained<MenuTarget>,
    // NSMenuItem targets are unretained. Keep the menu/items alive alongside
    // their target, then detach the menu before dropping either.
    _menu: Retained<NSMenu>,
}

impl StatusMenu {
    fn new(status: AppStatus, mtm: MainThreadMarker) -> Self {
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);
        let mut actions = Vec::new();
        let mut row = |title: &str, tag| {
            let row = NSMenuItem::new(mtm);
            row.setTitle(&NSString::from_str(title));
            row.setEnabled(tag != 0);
            row.setTag(tag);
            if tag != 0 {
                actions.push(row.clone());
            }
            menu.addItem(&row);
            row
        };
        let service = row("Service: Starting…", 0);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        let accessibility = row("Accessibility: Checking…", 0);
        row("Read and operate application controls.", 0);
        let request_accessibility = row("Request Accessibility…", 2);
        row("Open Accessibility Settings…", 3);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        let screen_recording = row("Screen Recording: Checking…", 0);
        row("Capture screenshots and recognize text.", 0);
        let request_screen = row("Request Screen Recording…", 4);
        row("Open Screen Recording Settings…", 5);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        row("Refresh Status", 1);
        row("After changing a permission, refresh and retry Nova.", 0);
        row("Bodhi can remain open.", 0);
        let notice = row("", 0);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        row("Quit Nova", 6);
        let controller = Controller::new(status.clone(), SystemPermissions);
        let this = MenuTarget::alloc(mtm).set_ivars(MenuIvars {
            status,
            controller,
            service,
            accessibility,
            screen_recording,
            request_accessibility,
            request_screen,
            notice,
            item: item.clone(),
            previous: RefCell::new(None),
            quit: Cell::new(false),
        });
        // SAFETY: NSObject's init signature is correct; this consumes the
        // allocated instance whose Rust ivars have just been initialized.
        let target: Retained<MenuTarget> = unsafe { msg_send![super(this), init] };
        for action in actions {
            // SAFETY: target lives until after its menu is detached; selector
            // is declared above with the matching NSMenuItem argument.
            unsafe {
                action.setTarget(Some(&target));
                action.setAction(Some(sel!(novaMenuAction:)));
            }
        }
        item.setMenu(Some(&menu));
        target.render();
        Self {
            item,
            target,
            _menu: menu,
        }
    }
}

impl Drop for StatusMenu {
    fn drop(&mut self) {
        self.item.setMenu(None);
        NSStatusBar::systemStatusBar().removeStatusItem(&self.item);
    }
}

/// The app's only UI lifecycle: start its existing service, keep a failed
/// service visible for diagnosis, and stop on explicit Quit. A duplicate
/// instance exits immediately, preserving the existing singleton owner.
pub fn run(runtime: &tokio::runtime::Runtime) -> Result<()> {
    let mtm = MainThreadMarker::new().context("Nova menu requires the process main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app.finishLaunching();
    let status = AppStatus::default();
    let menu = StatusMenu::new(status.clone(), mtm);
    let service_status = status.clone();
    let mut service = Some(
        runtime.spawn(async move { crate::app_service::run_with_status(service_status).await }),
    );
    while !menu.target.ivars().quit.get() {
        if service.as_ref().is_some_and(|task| task.is_finished()) {
            match runtime.block_on(service.take().expect("finished service exists")) {
                Ok(Ok(crate::app_service::ServiceExit::AlreadyRunning)) => return Ok(()),
                Ok(Err(error)) => tracing::error!(%error, "Nova app service failed"),
                Err(error) => tracing::error!(%error, "Nova app service task failed"),
            }
            status.set_service(ServiceState::Failed);
        }
        menu.target.render();
        super::event_loop::pump_application(&app);
    }
    if let Some(service) = service {
        service.abort();
        // Let the existing listener guard release the owned socket. Runtime
        // shutdown then ends per-connector sessions; it does not replay them.
        let _ = runtime.block_on(service);
    }
    Ok(())
}
