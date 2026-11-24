//! Pictures for the Markdown preview: local PNG and JPEG files only,
//! decoded by `aster-image` like the pictures in workbooks and slides.
//!
//! Nothing is fetched from the network. A web address in a document would
//! tell its server that the document was opened, and where; so web images
//! stay boxes with their description. The same goes for files on network
//! shares: `\\host\share` and `//host/share` paths are refused, and so is
//! any path that passes through a folder that is not on a local disk (an
//! NFS or SMB mount, macOS's `/net` automounts), checked before that folder
//! is entered so that not even the mount is triggered. This holds for
//! addresses an assistant or MCP client writes into a document too: they
//! go through the same check. Paths are taken relative to the
//! document's folder, the way other Markdown tools read them; absolute paths
//! and `file:` URLs work too.
//!
//! Decoded pictures are kept between layouts, keyed by path and checked
//! against the file's modification time and size, so typing does not decode
//! them again.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use vello_cpu::Pixmap;

/// Files larger than this are not read.
const MAX_FILE: u64 = 32 * 1024 * 1024;
/// Pictures larger than this are shrunk when decoded: the preview is never
/// wider than a page.
const MAX_SIDE: u32 = 4096;

/// Where a picture's address points on this computer, if it is a local
/// file: a path relative to `base`, an absolute path, or a `file:` URL.
/// `%20` and other escapes are decoded; `<…>` around the address is
/// dropped. Web addresses, `data:` and other schemes give `None`.
pub fn resolve(base: Option<&Path>, url: &str) -> Option<PathBuf> {
    let url = url.trim();
    let url = url
        .strip_prefix('<')
        .and_then(|u| u.strip_suffix('>'))
        .unwrap_or(url);
    // Drop a `#fragment` or `?query`: they mean nothing for a file.
    let url = url.split(['#', '?']).next().unwrap_or(url);
    if url.is_empty() {
        return None;
    }
    // A network share by its UNC name, written either way round.
    if url.starts_with("\\\\") || url.starts_with("//") {
        return None;
    }
    let path = if let Some(rest) = url.strip_prefix("file://") {
        // file:///abs/path; a host part other than localhost is not ours.
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        if !rest.starts_with('/') {
            return None;
        }
        let rest = decode(rest)?;
        // file:////host/share is a network share too.
        if rest.starts_with("//") || rest.starts_with("\\\\") {
            return None;
        }
        PathBuf::from(rest)
    } else if has_scheme(url) {
        return None;
    } else {
        let decoded = PathBuf::from(decode(url)?);
        if decoded.is_absolute() {
            decoded
        } else {
            base?.join(decoded)
        }
    };
    Some(path)
}

/// Whether an address starts with a URL scheme (`https:`, `data:`, …). A
/// Windows drive letter (`C:\`) is not one.
fn has_scheme(url: &str) -> bool {
    let Some(colon) = url.find(':') else {
        return false;
    };
    let scheme = &url[..colon];
    scheme.len() > 1
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// Percent-decoding; `None` if the result is not UTF-8.
fn decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%'
            && at + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex(bytes[at + 1]), hex(bytes[at + 2]))
        {
            out.push(high * 16 + low);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex(byte: u8) -> Option<u8> {
    (byte as char).to_digit(16).map(|d| d as u8)
}

/// Decode PNG or JPEG bytes into a premultiplied pixmap.
pub fn decode_picture(bytes: &[u8]) -> Option<Pixmap> {
    aster_image::decode(bytes, MAX_SIDE)
        .ok()
        .and_then(crate::picture::to_pixmap)
}

/// Pictures by path, decoded once.
#[derive(Default)]
pub struct Pictures {
    base: Option<PathBuf>,
    cache: HashMap<PathBuf, Entry>,
}

struct Entry {
    modified: Option<SystemTime>,
    len: u64,
    /// `None`: the file is not a picture we can show.
    picture: Option<Arc<Pixmap>>,
}

impl Pictures {
    /// The folder relative addresses start from: the document's folder, or
    /// `None` for a document that was never saved.
    pub fn set_base(&mut self, base: Option<PathBuf>) {
        self.base = base;
    }

    /// The picture at `url`, if it is a local PNG or JPEG file that can be
    /// read.
    pub fn get(&mut self, url: &str) -> Option<Arc<Pixmap>> {
        let path = resolve(self.base.as_deref(), url)?;
        if !aster_platform::volumes::on_local_disks(&path) {
            return None;
        }
        let meta = std::fs::metadata(&path).ok()?;
        if !meta.is_file() || meta.len() > MAX_FILE {
            return None;
        }
        let modified = meta.modified().ok();
        if let Some(entry) = self.cache.get(&path)
            && entry.modified == modified
            && entry.len == meta.len()
        {
            return entry.picture.clone();
        }
        let picture = std::fs::read(&path)
            .ok()
            .and_then(|bytes| decode_picture(&bytes))
            .map(Arc::new);
        // A handful of documents' pictures; drop them all rather than grow.
        if self.cache.len() > 256 {
            self.cache.clear();
        }
        self.cache.insert(
            path,
            Entry {
                modified,
                len: meta.len(),
                picture: picture.clone(),
            },
        );
        picture
    }
}

/// A PNG of one colour, for tests.
#[cfg(test)]
pub(crate) fn png_bytes(width: u32, height: u32, pixel: [u8; 4]) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        let data: Vec<u8> = (0..width * height).flat_map(|_| pixel).collect();
        writer.write_image_data(&data).unwrap();
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_addresses_resolve_and_web_ones_do_not() {
        let base = Path::new("/docs/notes");
        assert_eq!(
            resolve(Some(base), "img/a.png"),
            Some(PathBuf::from("/docs/notes/img/a.png"))
        );
        assert_eq!(
            resolve(Some(base), "../图片/b%20c.jpg"),
            Some(PathBuf::from("/docs/notes/../图片/b c.jpg"))
        );
        assert_eq!(
            resolve(Some(base), "<my pic.png>"),
            Some(PathBuf::from("/docs/notes/my pic.png"))
        );
        assert_eq!(
            resolve(Some(base), "/abs/x.png#frag"),
            Some(PathBuf::from("/abs/x.png"))
        );
        assert_eq!(
            resolve(None, "file:///abs/%E5%9B%BE.png"),
            Some(PathBuf::from("/abs/图.png"))
        );
        assert_eq!(resolve(Some(base), "https://example.com/a.png"), None);
        assert_eq!(resolve(Some(base), "data:image/png;base64,AAAA"), None);
        assert_eq!(resolve(Some(base), "file://server/share/a.png"), None);
        // Network shares by name, however they are written.
        assert_eq!(resolve(Some(base), "\\\\server\\share\\a.png"), None);
        assert_eq!(resolve(Some(base), "//server/share/a.png"), None);
        assert_eq!(resolve(Some(base), "file:////server/share/a.png"), None);
        assert_eq!(resolve(Some(base), "file://%2F%2Fserver/a.png"), None);
        // A document never saved has no folder to start from.
        assert_eq!(resolve(None, "a.png"), None);
        assert_eq!(resolve(Some(base), ""), None);
    }

    fn png(width: u32, height: u32, pixel: [u8; 4]) -> Vec<u8> {
        png_bytes(width, height, pixel)
    }

    #[test]
    fn pictures_are_decoded_premultiplied() {
        let pixmap = decode_picture(&png(3, 2, [200, 100, 50, 128])).unwrap();
        assert_eq!((pixmap.width(), pixmap.height()), (3, 2));
        let first = pixmap.data()[0];
        assert_eq!((first.r, first.g, first.b, first.a), (100, 50, 25, 128));
        assert!(decode_picture(b"not a picture").is_none());
    }

    #[test]
    fn pictures_are_read_once_and_again_when_the_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.png"), png(4, 4, [0, 0, 0, 255])).unwrap();
        std::fs::write(dir.path().join("broken.png"), b"xx").unwrap();
        let mut pictures = Pictures::default();
        pictures.set_base(Some(dir.path().to_owned()));
        let first = pictures.get("a.png").unwrap();
        let again = pictures.get("a.png").unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        std::fs::write(dir.path().join("a.png"), png(8, 2, [0, 0, 0, 255])).unwrap();
        let changed = pictures.get("a.png").unwrap();
        assert_eq!(changed.width(), 8);
        assert!(pictures.get("broken.png").is_none());
        assert!(pictures.get("missing.png").is_none());
    }

    #[test]
    fn pictures_on_local_disks_are_read_and_network_mounts_are_not() {
        let dir = tempfile::tempdir().unwrap();
        assert!(aster_platform::volumes::on_local_disks(
            &dir.path().join("a.png")
        ));
        // macOS mounts home folders on demand through autofs, which is not
        // a local file system: refused before anything inside is touched.
        #[cfg(target_os = "macos")]
        {
            let automount = Path::new("/System/Volumes/Data/home");
            if automount.exists() {
                assert!(!aster_platform::volumes::on_local_disks(
                    &automount.join("someone/pic.png")
                ));
            }
        }
    }
}
