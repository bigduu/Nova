//! In-process service and permission status. No Chrome commands, IPC, capture
//! operations, or implicit permission requests belong in this model.

use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceState {
    Starting,
    Ready,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    Accessibility,
    ScreenRecording,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionState {
    NotChecked,
    Granted,
    NotGranted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub service: ServiceState,
    pub accessibility: PermissionState,
    pub screen_recording: PermissionState,
    pub notice: Option<String>,
}

#[derive(Clone)]
pub struct AppStatus(Arc<Mutex<Snapshot>>);

impl Default for AppStatus {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(Snapshot {
            service: ServiceState::Starting,
            accessibility: PermissionState::NotChecked,
            screen_recording: PermissionState::NotChecked,
            notice: None,
        })))
    }
}

impl AppStatus {
    pub fn snapshot(&self) -> Snapshot {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_service(&self, service: ServiceState) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).service = service;
    }

    fn permissions(&self, accessibility: bool, screen_recording: bool) {
        let state = |granted| {
            if granted {
                PermissionState::Granted
            } else {
                PermissionState::NotGranted
            }
        };
        let mut snapshot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        snapshot.accessibility = state(accessibility);
        snapshot.screen_recording = state(screen_recording);
    }

    fn notice(&self, notice: Option<String>) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).notice = notice;
    }
}

/// Separate passive checks from explicit actions so UI setup/refresh cannot
/// accidentally turn into a request for either permission.
pub trait Permissions {
    fn check(&self, permission: Permission) -> bool;
    fn request(&self, permission: Permission);
    fn open_settings(&self, permission: Permission) -> Result<(), String>;
}

pub struct Controller<P> {
    status: AppStatus,
    permissions: P,
}

impl<P: Permissions> Controller<P> {
    pub fn new(status: AppStatus, permissions: P) -> Self {
        let controller = Self {
            status,
            permissions,
        };
        controller.refresh();
        controller
    }

    pub fn refresh(&self) {
        // Never hold the status lock while calling an OS API. Service state
        // remains independently readable/updatable during permission checks.
        let accessibility = self.permissions.check(Permission::Accessibility);
        let screen_recording = self.permissions.check(Permission::ScreenRecording);
        self.status.permissions(accessibility, screen_recording);
        self.status.notice(None);
    }

    pub fn request(&self, permission: Permission) {
        self.permissions.request(permission);
        self.refresh();
        self.status.notice(Some(
            "After changing a permission in Settings, choose Refresh Status.".into(),
        ));
    }

    pub fn open_settings(&self, permission: Permission) {
        self.status
            .notice(self.permissions.open_settings(permission).err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    #[derive(Default)]
    struct Fake {
        calls: RefCell<Vec<(&'static str, Permission)>>,
        accessibility: Cell<bool>,
        screen_recording: Cell<bool>,
    }

    impl Permissions for Rc<Fake> {
        fn check(&self, permission: Permission) -> bool {
            self.calls.borrow_mut().push(("check", permission));
            match permission {
                Permission::Accessibility => self.accessibility.get(),
                Permission::ScreenRecording => self.screen_recording.get(),
            }
        }
        fn request(&self, permission: Permission) {
            self.calls.borrow_mut().push(("request", permission));
        }
        fn open_settings(&self, permission: Permission) -> Result<(), String> {
            self.calls.borrow_mut().push(("settings", permission));
            Ok(())
        }
    }

    #[test]
    fn startup_refresh_and_service_readiness_do_not_request_permissions() {
        let status = AppStatus::default();
        let adapter = Rc::new(Fake::default());
        let controller = Controller::new(status.clone(), adapter.clone());
        assert_eq!(status.snapshot().service, ServiceState::Starting);
        assert_eq!(
            status.snapshot().screen_recording,
            PermissionState::NotGranted
        );
        status.set_service(ServiceState::Ready);
        adapter.accessibility.set(true);
        controller.refresh();
        let snapshot = status.snapshot();
        assert_eq!(snapshot.service, ServiceState::Ready);
        assert_eq!(snapshot.accessibility, PermissionState::Granted);
        assert_eq!(snapshot.screen_recording, PermissionState::NotGranted);
        assert!(adapter
            .calls
            .borrow()
            .iter()
            .all(|(operation, _)| *operation == "check"));
        adapter.screen_recording.set(true);
        controller.refresh();
        assert_eq!(status.snapshot().screen_recording, PermissionState::Granted);
        status.set_service(ServiceState::Failed);
        controller.refresh();
        assert_eq!(status.snapshot().service, ServiceState::Failed);
    }

    #[test]
    fn explicit_actions_affect_only_the_selected_permission() {
        for permission in [Permission::Accessibility, Permission::ScreenRecording] {
            let adapter = Rc::new(Fake::default());
            let controller = Controller::new(AppStatus::default(), adapter.clone());
            adapter.calls.borrow_mut().clear();
            controller.request(permission);
            let requests: Vec<_> = adapter
                .calls
                .borrow()
                .iter()
                .filter(|(operation, _)| *operation != "check")
                .copied()
                .collect();
            assert_eq!(requests, [("request", permission)]);
            adapter.calls.borrow_mut().clear();
            controller.open_settings(permission);
            assert_eq!(*adapter.calls.borrow(), [("settings", permission)]);
        }
    }

    #[test]
    fn a_slow_permission_check_does_not_lock_service_status() {
        use std::sync::mpsc;
        use std::time::Duration;
        struct Waiting {
            entered: mpsc::Sender<()>,
            release: mpsc::Receiver<()>,
        }
        impl Permissions for Waiting {
            fn check(&self, permission: Permission) -> bool {
                if permission == Permission::Accessibility {
                    self.entered.send(()).unwrap();
                    self.release.recv_timeout(Duration::from_secs(5)).unwrap();
                }
                false
            }
            fn request(&self, _: Permission) {
                panic!("unexpected permission request");
            }
            fn open_settings(&self, _: Permission) -> Result<(), String> {
                panic!("unexpected settings action");
            }
        }
        let status = AppStatus::default();
        let checker_status = status.clone();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let checker = std::thread::spawn(move || {
            Controller::new(
                checker_status,
                Waiting {
                    entered: entered_tx,
                    release: release_rx,
                },
            );
        });
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let updater_status = status.clone();
        let (updated_tx, updated_rx) = mpsc::channel();
        let updater = std::thread::spawn(move || {
            updater_status.set_service(ServiceState::Ready);
            updated_tx.send(updater_status.snapshot()).unwrap();
        });
        let updated = updated_rx.recv_timeout(Duration::from_secs(1));
        release_tx.send(()).unwrap();
        checker.join().unwrap();
        updater.join().unwrap();
        assert_eq!(updated.unwrap().service, ServiceState::Ready);
        assert_eq!(status.snapshot().service, ServiceState::Ready);
    }
}
