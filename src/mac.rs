//! Thin wrappers over the macOS APIs the sampler needs. Everything here
//! degrades gracefully: no TCC permission is required except for window
//! titles, which silently come back NULL until Screen Recording is granted.

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use objc2_app_kit::NSWorkspace;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventSourceSecondsSinceLastEventType(state: i32, event_type: u32) -> f64;
    fn CGWindowListCopyWindowInfo(
        option: u32,
        relative_to_window: u32,
    ) -> core_foundation::array::CFArrayRef;
    fn CGSessionCopyCurrentDictionary() -> core_foundation::dictionary::CFDictionaryRef;
}

const K_CG_EVENT_SOURCE_STATE_HID_SYSTEM: i32 = 1;
const K_CG_ANY_INPUT_EVENT_TYPE: u32 = u32::MAX;
const K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY: u32 = 1 << 0;
const K_CG_WINDOW_LIST_EXCLUDE_DESKTOP_ELEMENTS: u32 = 1 << 4;

/// Milliseconds since the last keyboard/mouse/trackpad input, system-wide.
pub fn idle_ms() -> u64 {
    let secs = unsafe {
        CGEventSourceSecondsSinceLastEventType(
            K_CG_EVENT_SOURCE_STATE_HID_SYSTEM,
            K_CG_ANY_INPUT_EVENT_TYPE,
        )
    };
    (secs * 1000.0) as u64
}

/// Whether the login session's screen is locked. Errs toward `true` when the
/// session dictionary is unavailable (e.g. fast user switching).
pub fn screen_locked() -> bool {
    let dict_ref = unsafe { CGSessionCopyCurrentDictionary() };
    if dict_ref.is_null() {
        return true;
    }
    let dict: CFDictionary<CFString, CFType> =
        unsafe { CFDictionary::wrap_under_create_rule(dict_ref) };
    dict.find(CFString::from_static_string("CGSSessionScreenIsLocked"))
        .map(|v| {
            v.downcast::<CFBoolean>()
                .map(bool::from)
                .unwrap_or(true)
        })
        .unwrap_or(false)
}

pub struct FrontmostApp {
    pub bundle_id: Option<String>,
    pub name: Option<String>,
    pub pid: i32,
}

/// The app that owns the menu bar right now, via NSWorkspace.
pub fn frontmost_app() -> Option<FrontmostApp> {
    let workspace = NSWorkspace::sharedWorkspace();
    let app = workspace.frontmostApplication()?;
    Some(FrontmostApp {
        bundle_id: app.bundleIdentifier().map(|s| s.to_string()),
        name: app.localizedName().map(|s| s.to_string()),
        pid: app.processIdentifier(),
    })
}

/// Title of the frontmost (layer 0) on-screen window owned by `pid`. Returns
/// None without Screen Recording permission: macOS then omits kCGWindowName
/// from the window info dictionaries rather than failing the call.
pub fn frontmost_window_title(pid: i32) -> Option<String> {
    let arr_ref = unsafe {
        CGWindowListCopyWindowInfo(
            K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY | K_CG_WINDOW_LIST_EXCLUDE_DESKTOP_ELEMENTS,
            0, // kCGNullWindowID
        )
    };
    if arr_ref.is_null() {
        return None;
    }
    let windows: CFArray<CFDictionary<CFString, CFType>> =
        unsafe { CFArray::wrap_under_create_rule(arr_ref.cast()) };

    let key_layer = CFString::from_static_string("kCGWindowLayer");
    let key_pid = CFString::from_static_string("kCGWindowOwnerPID");
    let key_name = CFString::from_static_string("kCGWindowName");

    for window in windows.iter() {
        let layer = window
            .find(&key_layer)
            .and_then(|v| v.downcast::<CFNumber>())
            .and_then(|n| n.to_i64());
        if layer != Some(0) {
            continue;
        }
        let owner = window
            .find(&key_pid)
            .and_then(|v| v.downcast::<CFNumber>())
            .and_then(|n| n.to_i64());
        if owner != Some(i64::from(pid)) {
            continue;
        }
        return window
            .find(&key_name)
            .and_then(|v| v.downcast::<CFString>())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());
    }
    None
}
