//! macOS-only window behaviour that neither Slint nor winit exposes.
//!
//! This belongs in `aster-platform` once that crate exists.

use std::ffi::{c_char, c_void};

use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::winit_030::winit::window::Window;

#[link(name = "objc", kind = "dylib")]
unsafe extern "C" {
    /// Declared without arguments: it must be cast to the exact signature of
    /// each method it calls, as Apple requires on arm64.
    fn objc_msgSend();
    fn object_getClass(object: *mut c_void) -> *mut c_void;
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn class_replaceMethod(
        class: *mut c_void,
        selector: *mut c_void,
        implementation: extern "C" fn(*mut c_void, *mut c_void) -> ObjcBool,
        types: *const c_char,
    ) -> *mut c_void;
}

/// The window's content view: the NSView winit draws into.
fn content_view(window: &Window) -> Option<*mut c_void> {
    let handle = window.window_handle().ok()?;
    match handle.as_raw() {
        RawWindowHandle::AppKit(handle) => Some(handle.ns_view.as_ptr()),
        _ => None,
    }
}

/// Make the system input method drop the composition it is holding.
///
/// winit's `set_ime_allowed(false)` forgets the composition on winit's side
/// but does not tell the input method, so its candidate window stays on screen
/// and follows the caret into whatever document is shown next. This asks the
/// view's text input context to discard the marked text, which is what AppKit
/// text views do when they give up the keyboard.
pub fn discard_marked_text(window: &Window) {
    let Some(view) = content_view(window) else {
        return;
    };
    type Send0 = unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void;
    type SendVoid = unsafe extern "C" fn(*mut c_void, *mut c_void);
    // SAFETY: `view` is winit's live NSView and this runs on the main thread.
    // `inputContext` takes no arguments and returns an object or nil;
    // `discardMarkedText` takes none and returns nothing. Each call goes
    // through `objc_msgSend` cast to exactly that signature.
    unsafe {
        let send_object: Send0 = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let context = send_object(view, sel_registerName(c"inputContext".as_ptr()));
        if context.is_null() {
            return;
        }
        let send_void: SendVoid = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        send_void(context, sel_registerName(c"discardMarkedText".as_ptr()));
    }
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
    let Some(view) = content_view(window) else {
        return;
    };
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
