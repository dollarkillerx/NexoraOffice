//! macOS-only window behaviour that neither Slint nor winit exposes.
//!
//! This belongs in `aster-platform` once that crate exists.

use std::ffi::{c_char, c_void};

use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::winit_030::winit::window::Window;

#[link(name = "objc", kind = "dylib")]
unsafe extern "C" {
    fn object_getClass(object: *mut c_void) -> *mut c_void;
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn class_replaceMethod(
        class: *mut c_void,
        selector: *mut c_void,
        implementation: extern "C" fn(*mut c_void, *mut c_void) -> ObjcBool,
        types: *const c_char,
    ) -> *mut c_void;
}

/// Objective-C `BOOL`: a real `bool` on Apple silicon, a signed char on Intel.
#[cfg(target_arch = "aarch64")]
type ObjcBool = bool;
#[cfg(not(target_arch = "aarch64"))]
type ObjcBool = i8;

/// Type encoding of a method that takes nothing and returns `BOOL`.
#[cfg(target_arch = "aarch64")]
const RETURNS_BOOL: &std::ffi::CStr = c"B@:";
#[cfg(not(target_arch = "aarch64"))]
const RETURNS_BOOL: &std::ffi::CStr = c"c@:";

extern "C" fn mouse_down_cannot_move_window(
    _this: *mut c_void,
    _selector: *mut c_void,
) -> ObjcBool {
    ObjcBool::default()
}

/// Stop AppKit from treating clicks on the content view as clicks on the title
/// bar.
///
/// The tab strip is drawn under a transparent title bar. winit's content view
/// is not opaque, so AppKit assumes a press on it may move the window: inside
/// the title-bar strip it starts a window drag from a tab, and a double click on
/// a tab zooms the window. Answering "no" to `mouseDownCanMoveWindow` leaves
/// both decisions to the shell, which drags and zooms from the empty part of
/// the strip only.
///
/// The method is replaced on the class of winit's view, which this process
/// uses for every window it opens.
pub fn claim_title_bar_clicks(window: &Window) {
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };
    let view = handle.ns_view.as_ptr();
    // SAFETY: `view` is the live NSView winit created for this window, and this
    // runs on the main thread, where winit only hands out window handles. The
    // replacement has the signature its type encoding declares, and the method
    // it replaces is a plain query with no side effects.
    unsafe {
        let class = object_getClass(view);
        if class.is_null() {
            return;
        }
        let selector = sel_registerName(c"mouseDownCanMoveWindow".as_ptr());
        class_replaceMethod(
            class,
            selector,
            mouse_down_cannot_move_window,
            RETURNS_BOOL.as_ptr(),
        );
    }
}
