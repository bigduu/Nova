//! The only menu adapter allowed to request macOS permissions. Checks never
//! enter ScreenCaptureKit or the capture helper, and never prompt.

use crate::app_status::{Permission, Permissions};
use core_foundation::{
    base::TCFType, boolean::CFBoolean, dictionary::CFDictionary, string::CFString,
};
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSString, NSURL};

pub(super) struct SystemPermissions;

impl Permissions for SystemPermissions {
    fn check(&self, permission: Permission) -> bool {
        match permission {
            // SAFETY: argless, passive TCC check only.
            Permission::Accessibility => unsafe { accessibility_sys::AXIsProcessTrusted() },
            Permission::ScreenRecording => super::geometry::preflight_screen_capture(),
        }
    }

    fn request(&self, permission: Permission) {
        match permission {
            Permission::Accessibility => {
                // SAFETY: Apple owns the static option key; the CF dictionary
                // retains the key/value throughout this explicit user request.
                unsafe {
                    let key = CFString::wrap_under_get_rule(
                        accessibility_sys::kAXTrustedCheckOptionPrompt,
                    );
                    let options = CFDictionary::from_CFType_pairs(&[(
                        key.as_CFType(),
                        CFBoolean::true_value().as_CFType(),
                    )]);
                    accessibility_sys::AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef());
                }
            }
            Permission::ScreenRecording => {
                super::geometry::request_screen_recording_access();
            }
        }
    }

    fn open_settings(&self, permission: Permission) -> Result<(), String> {
        let pane = match permission {
            Permission::Accessibility => "Privacy_Accessibility",
            Permission::ScreenRecording => "Privacy_ScreenCapture",
        };
        let url = NSURL::URLWithString(&NSString::from_str(&format!(
            "x-apple.systempreferences:com.apple.preference.security?{pane}"
        )))
        .ok_or("Could not open Privacy & Security settings.")?;
        if NSWorkspace::sharedWorkspace().openURL(&url) {
            Ok(())
        } else {
            Err("Open System Settings → Privacy & Security to change this permission.".into())
        }
    }
}
