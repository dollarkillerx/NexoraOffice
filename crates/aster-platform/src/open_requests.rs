//! Files the system asks the application to open.
//!
//! On macOS, double-clicking a file in Finder, "Open With" and dropping a file
//! on the Dock icon all send the running application (or start it with) an
//! open-documents request, which names the files as `file://` URLs. This
//! module turns those into paths and hands them to the shell.
//!
//! The request can arrive before the window exists, so it is queued until the
//! shell attaches a handler; what was queued is then delivered once, in order.
//! Other systems pass files as command-line arguments and never use this.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

type Handler = Box<dyn FnMut(Vec<PathBuf>) + Send>;

/// Where open requests wait for the shell. One of these is global; tests make
/// their own.
#[derive(Default)]
pub struct OpenRequests {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    handler: Option<Handler>,
    pending: Vec<PathBuf>,
}

impl OpenRequests {
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                handler: None,
                pending: Vec::new(),
            }),
        }
    }

    /// A request for these files. With no handler yet they are queued, each
    /// path once.
    pub fn deliver(&self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match inner.handler.as_mut() {
            Some(handler) => handler(paths),
            None => {
                for path in paths {
                    if !inner.pending.contains(&path) {
                        inner.pending.push(path);
                    }
                }
            }
        }
    }

    /// From now on requests go to `handler`, and what was queued goes to it
    /// right away. `already_open` are paths the application opens by itself
    /// (those on its command line): they are dropped from the queue, so that
    /// a file is not opened twice. `handler` runs under a lock and must not
    /// call back into this.
    pub fn attach(
        &self,
        mut handler: impl FnMut(Vec<PathBuf>) + Send + 'static,
        already_open: &[PathBuf],
    ) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut queued = std::mem::take(&mut inner.pending);
        queued.retain(|path| !already_open.contains(path));
        if !queued.is_empty() {
            handler(queued);
        }
        inner.handler = Some(Box::new(handler));
    }
}

static REQUESTS: OpenRequests = OpenRequests::new();

/// The requests of this process.
pub fn requests() -> &'static OpenRequests {
    &REQUESTS
}

/// Start receiving the system's open requests. Call it after the windowing
/// system's event loop has been created and before it runs. False when this
/// system does not send them or the hook could not be installed.
pub fn install() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::install()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// The paths of the `file://` URLs among `urls`, in order. Anything else
/// (web links, a remote host, a malformed escape) is ignored.
pub fn paths_from_urls<'a>(urls: impl IntoIterator<Item = &'a str>) -> Vec<PathBuf> {
    urls.into_iter().filter_map(path_from_file_url).collect()
}

/// The path a `file://` URL names, with the percent escapes decoded.
pub fn path_from_file_url(url: &str) -> Option<PathBuf> {
    let scheme = url.get(..5)?;
    if !scheme.eq_ignore_ascii_case("file:") {
        return None;
    }
    let rest = url[5..].strip_prefix("//")?;
    // The authority is empty or `localhost`; the path starts at the next slash.
    let slash = rest.find('/')?;
    let (host, path) = rest.split_at(slash);
    if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
        return None;
    }
    // A query or fragment is not part of a file name.
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let bytes = percent_decode(path)?;
    if bytes.contains(&0) {
        return None;
    }
    to_path(bytes)
}

fn percent_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let pair = bytes.get(at + 1..at + 3)?;
            let high = (pair[0] as char).to_digit(16)?;
            let low = (pair[1] as char).to_digit(16)?;
            out.push((high * 16 + low) as u8);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    Some(out)
}

#[cfg(unix)]
fn to_path(bytes: Vec<u8>) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

#[cfg(not(unix))]
fn to_path(bytes: Vec<u8>) -> Option<PathBuf> {
    String::from_utf8(bytes).ok().map(PathBuf::from)
}

/// `path` in the absolute form the queue compares: the one the shell makes of
/// its command-line files.
pub fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(target_os = "macos")]
mod macos {
    //! winit owns the application delegate and does not implement
    //! `application:openURLs:`, so AppKit would answer every open request
    //! with an error. The delegate class is not ours to subclass, but
    //! Objective-C lets a method be added to it at run time, before the event
    //! loop starts; AppKit asks the delegate which methods it has each time a
    //! request comes in.

    use std::ffi::{c_char, c_void};

    use objc2_foundation::{NSArray, NSURL};

    use super::{paths_from_urls, requests};

    type OpenUrls = extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut NSArray<NSURL>);

    #[link(name = "objc", kind = "dylib")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> *mut c_void;
        fn sel_registerName(name: *const c_char) -> *mut c_void;
        fn class_addMethod(
            class: *mut c_void,
            selector: *mut c_void,
            implementation: OpenUrls,
            types: *const c_char,
        ) -> bool;
    }

    pub fn install() -> bool {
        // SAFETY: plain runtime lookups with NUL-terminated names. The type
        // encoding `v@:@@` is a method returning nothing that takes the
        // receiver, the selector and two objects, which is what
        // `application:openURLs:` is and `open_urls` implements. The class is
        // winit's `NSApplicationDelegate`.
        unsafe {
            let class = objc_getClass(c"WinitApplicationDelegate".as_ptr());
            if class.is_null() {
                return false;
            }
            class_addMethod(
                class,
                sel_registerName(c"application:openURLs:".as_ptr()),
                open_urls,
                c"v@:@@".as_ptr(),
            )
        }
    }

    /// `-[NSApplicationDelegate application:openURLs:]`, on the main thread.
    extern "C" fn open_urls(
        _this: *mut c_void,
        _selector: *mut c_void,
        _application: *mut c_void,
        urls: *mut NSArray<NSURL>,
    ) {
        // SAFETY: AppKit passes a live NSArray of NSURL for the call.
        let Some(urls) = (unsafe { urls.as_ref() }) else {
            return;
        };
        let strings: Vec<String> = urls
            .iter()
            .filter_map(|url| url.absoluteString())
            .map(|text| text.to_string())
            .collect();
        requests().deliver(paths_from_urls(strings.iter().map(String::as_str)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn p(text: &str) -> PathBuf {
        PathBuf::from(text)
    }

    #[test]
    fn only_file_urls_become_paths() {
        let urls = [
            "file:///Users/a/doc.pdf",
            "https://example.com/doc.pdf",
            "mailto:a@example.com",
            "file://server/share/doc.pdf",
            "not a url",
            "file:///tmp/b.md",
            "FILE://localhost/tmp/c.txt",
        ];
        assert_eq!(
            paths_from_urls(urls),
            vec![p("/Users/a/doc.pdf"), p("/tmp/b.md"), p("/tmp/c.txt")]
        );
    }

    #[test]
    fn escapes_and_non_ascii_names_are_decoded() {
        assert_eq!(
            path_from_file_url("file:///Users/a/My%20Notes/%E5%B8%B3%E7%B0%BF%2B1.xlsx"),
            Some(p("/Users/a/My Notes/帳簿+1.xlsx"))
        );
        // Already decoded, as some senders write them.
        assert_eq!(
            path_from_file_url("file:///Users/a/報告書.pptx"),
            Some(p("/Users/a/報告書.pptx"))
        );
        // A percent sign that starts no escape is not a path.
        assert_eq!(path_from_file_url("file:///tmp/100%.md"), None);
        assert_eq!(path_from_file_url("file:///tmp/a%00b.md"), None);
        assert_eq!(
            path_from_file_url("file:///tmp/a.md#top"),
            Some(p("/tmp/a.md"))
        );
        assert_eq!(path_from_file_url("file:relative.md"), None);
    }

    type Seen = Arc<Mutex<Vec<Vec<PathBuf>>>>;

    fn sink() -> (Seen, impl FnMut(Vec<PathBuf>) + Send + 'static) {
        let seen: Seen = Arc::default();
        let inner = seen.clone();
        (seen, move |paths| inner.lock().unwrap().push(paths))
    }

    #[test]
    fn requests_before_the_window_wait_and_arrive_once_in_order() {
        let queue = OpenRequests::new();
        queue.deliver(vec![p("/a.md"), p("/b.md")]);
        queue.deliver(vec![p("/a.md"), p("/c.md")]);
        let (seen, handler) = sink();
        queue.attach(handler, &[]);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![vec![p("/a.md"), p("/b.md"), p("/c.md")]]
        );
        // Later requests go straight through, and nothing is delivered twice.
        queue.deliver(vec![p("/d.md")]);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[1], vec![p("/d.md")]);
    }

    #[test]
    fn command_line_files_are_not_opened_again() {
        let queue = OpenRequests::new();
        queue.deliver(vec![p("/a.md"), p("/b.md")]);
        let (seen, handler) = sink();
        queue.attach(handler, &[p("/a.md")]);
        assert_eq!(*seen.lock().unwrap(), vec![vec![p("/b.md")]]);

        let queue = OpenRequests::new();
        queue.deliver(vec![p("/a.md")]);
        let (seen, handler) = sink();
        queue.attach(handler, &[p("/a.md")]);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn nothing_queued_means_no_call() {
        let queue = OpenRequests::new();
        let (seen, handler) = sink();
        queue.attach(handler, &[]);
        queue.deliver(Vec::new());
        assert!(seen.lock().unwrap().is_empty());
    }
}
