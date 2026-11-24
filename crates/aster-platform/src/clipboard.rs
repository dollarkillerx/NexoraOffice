//! Pictures on the system clipboard, for pasting them into a document.
//!
//! Only the encoded picture is taken, as the program that copied it wrote
//! it (PNG or JPEG), so that it can be saved as a file without decoding and
//! encoding it again. A clipboard holding only other formats (TIFF, PDF)
//! gives nothing.

/// An encoded picture taken from the clipboard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardPicture {
    pub bytes: Vec<u8>,
    /// `png` or `jpg`: the file extension it is saved with.
    pub extension: &'static str,
}

/// The picture on the clipboard, if there is one in a format we keep.
#[cfg(target_os = "macos")]
pub fn picture() -> Option<ClipboardPicture> {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypePNG};
    use objc2_foundation::NSString;

    let board = NSPasteboard::generalPasteboard();
    // SAFETY: an AppKit constant, valid for the life of the program.
    let png = unsafe { NSPasteboardTypePNG };
    if let Some(data) = board.dataForType(png) {
        return checked(data.to_vec(), "png");
    }
    let jpeg = NSString::from_str("public.jpeg");
    board
        .dataForType(&jpeg)
        .and_then(|data| checked(data.to_vec(), "jpg"))
}

/// Not on other systems yet.
#[cfg(not(target_os = "macos"))]
pub fn picture() -> Option<ClipboardPicture> {
    None
}

/// The bytes, if they start the way their format does: what a program put
/// on the clipboard under a type is not trusted to be that type.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn checked(bytes: Vec<u8>, extension: &'static str) -> Option<ClipboardPicture> {
    let fits = match extension {
        "png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "jpg" => bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
        _ => false,
    };
    fits.then_some(ClipboardPicture { bytes, extension })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_bytes_of_the_named_format_are_taken() {
        assert!(checked(b"\x89PNG\r\n\x1a\nrest".to_vec(), "png").is_some());
        assert!(checked(vec![0xFF, 0xD8, 0xFF, 0xE0], "jpg").is_some());
        assert!(checked(b"GIF89a".to_vec(), "png").is_none());
        assert!(checked(b"\x89PNG\r\n\x1a\n".to_vec(), "jpg").is_none());
    }
}
