//! Opening, checking and rendering in this process, with files built to
//! hurt. What the child process adds (memory, time, killing) is tested in
//! `tests/process.rs`.

use std::io::Write;
use std::sync::mpsc;
use std::time::Duration;

use flate2::Compression;
use flate2::write::{DeflateEncoder, ZlibEncoder};
use hayro::RenderCache;

use super::*;
use crate::builder::{Builder, deflate, one_page, with_picture, zeros_deflated};

/// Run `f` on a thread with the child's stack, with a time limit.
fn on_worker_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (send, receive) = mpsc::channel();
    std::thread::Builder::new()
        .stack_size(STACK_BYTES)
        .spawn(move || {
            let _ = send.send(f());
        })
        .unwrap();
    receive
        .recv_timeout(Duration::from_secs(60))
        .expect("took more than a minute")
}

fn open_with(bytes: Vec<u8>, limits: Limits) -> Result<Info, OpenError> {
    on_worker_thread(move || Document::open(bytes, "", limits).map(|d| d.info().clone()))
}

fn open(bytes: Vec<u8>) -> Result<Info, OpenError> {
    open_with(bytes, Limits::default())
}

type Page = Option<(u32, u32, Vec<u8>)>;

/// Open and render every page; `None` for a page that could not be drawn.
fn render_all(bytes: Vec<u8>, scale: f32) -> Result<Vec<Page>, OpenError> {
    on_worker_thread(move || {
        let document = Document::open(bytes, "", Limits::default())?;
        let cache = RenderCache::new();
        Ok((0..document.info().pages.len())
            .map(|page| match document.render(&cache, page, scale) {
                Ok(Some(p)) => Some((
                    u32::from(p.width()),
                    u32::from(p.height()),
                    p.data_as_u8_slice().to_vec(),
                )),
                _ => None,
            })
            .collect())
    })
}

const STREAM: Result<Info, OpenError> = Err(OpenError::TooLarge(Exceeded::Stream));
const IMAGE: Result<Info, OpenError> = Err(OpenError::TooLarge(Exceeded::Image));
const UNCHECKED: Result<Info, OpenError> = Err(OpenError::Unchecked);

/// A one-page document whose content stream has `dict` and `data`.
fn content(dict: &str, data: &[u8]) -> Vec<u8> {
    one_page(|_| "<< >>".into(), dict, data)
}

/// 2 GB of zeros, deflated (a few megabytes).
fn bomb() -> Vec<u8> {
    zeros_deflated(2 << 30)
}

#[test]
fn a_page_opens_and_renders() {
    let bytes = Builder::document(&[b"1 0 0 rg 20 20 60 60 re f"]);
    let info = open(bytes.clone()).unwrap();
    assert_eq!(
        info.pages,
        vec![PageSize {
            width: 200.0,
            height: 100.0
        }]
    );
    assert!(!info.encrypted);
    let pages = render_all(bytes, 2.0).unwrap();
    let (width, height, pixels) = pages[0].clone().unwrap();
    assert_eq!((width, height), (400, 200));
    let pixel = |x: u32, y: u32| {
        let i = ((y * width + x) * 4) as usize;
        pixels[i..i + 4].to_vec()
    };
    assert_eq!(pixel(100, 100), vec![255, 0, 0, 255]);
    assert_eq!(pixel(300, 100), vec![255, 255, 255, 255]);
}

#[test]
fn rotated_pages_report_their_shown_size() {
    let mut b = Builder::default();
    let catalog = b.reserve();
    let pages = b.reserve();
    let content = b.stream("", b"");
    let page = b.add(format!(
        "<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 200 100] /Rotate 90 /Contents {content} 0 R >>"
    ));
    b.set(catalog, format!("<< /Type /Catalog /Pages {pages} 0 R >>"));
    b.set(
        pages,
        format!("<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>"),
    );
    let info = open(b.finish(catalog)).unwrap();
    assert_eq!(
        info.pages,
        vec![PageSize {
            width: 100.0,
            height: 200.0
        }]
    );
}

#[test]
fn ordinary_filters_pass() {
    let text = b"0 0 1 rg 10 10 50 50 re f";
    assert!(open(content("/Filter /FlateDecode", &deflate(text))).is_ok());
    let hex: Vec<u8> = deflate(text)
        .iter()
        .flat_map(|b| format!("{b:02x}").into_bytes())
        .collect();
    assert!(open(content("/Filter [/AHx /Fl]", &hex)).is_ok());
    // PNG "None" predictor, rows of 5 bytes.
    let mut rows = Vec::new();
    for chunk in text.chunks(5) {
        rows.push(0);
        rows.extend_from_slice(chunk);
    }
    let dict = "/Filter /FlateDecode /DecodeParms << /Predictor 12 /Columns 5 >>";
    assert!(open(content(dict, &deflate(&rows))).is_ok());
}

#[test]
fn not_a_pdf() {
    assert_eq!(open(Vec::new()), Err(OpenError::Damaged));
    assert_eq!(open(b"hello, world".to_vec()), Err(OpenError::Damaged));
    let mut noise = Vec::new();
    let mut x = 12345u32;
    for _ in 0..200_000 {
        x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
        noise.push((x >> 16) as u8);
    }
    assert!(open(noise).is_err());
}

#[test]
fn every_truncation_of_a_file_is_survived() {
    let bytes = Builder::document(&[b"0 0 1 rg 10 10 50 50 re f", b"BT /F1 12 Tf (x) Tj ET"]);
    for end in (0..bytes.len()).step_by(7) {
        if let Ok(info) = open(bytes[..end].to_vec()) {
            assert!(!info.pages.is_empty());
        }
    }
}

#[test]
fn a_stream_that_inflates_to_gigabytes_is_refused() {
    assert_eq!(open(content("/Filter /FlateDecode", &bomb())), STREAM);
}

#[test]
fn the_short_filter_key_is_read() {
    // The parser reads /F before /Filter.
    assert_eq!(open(content("/F /FlateDecode", &bomb())), STREAM);
    assert_eq!(open(content("/F [/FlateDecode]", &bomb())), STREAM);
}

#[test]
fn crypt_and_unknown_names_before_a_filter() {
    // The parser skips a leading /Crypt and drops names it does not know.
    assert_eq!(
        open(content("/Filter [/Crypt /FlateDecode]", &bomb())),
        STREAM
    );
    assert_eq!(
        open(content("/Filter [/Bogus /FlateDecode]", &bomb())),
        STREAM
    );
    assert_eq!(
        open(content(
            "/Filter [/FlateDecode /Bogus /FlateDecode]",
            &deflate(&bomb())
        )),
        STREAM
    );
    // Crypt anywhere else is not decoded by the parser either: refused.
    assert_eq!(
        open(content("/Filter [/FlateDecode /Crypt]", &deflate(b"0 g"))),
        UNCHECKED
    );
}

#[test]
fn escaped_filter_names() {
    assert_eq!(open(content("/Filter /Fl#61teDecode", &bomb())), STREAM);
    assert_eq!(open(content("/Filter [/#46l]", &bomb())), STREAM);
}

#[test]
fn a_filter_through_a_reference() {
    let bytes = one_page(
        |b| {
            b.add("/FlateDecode");
            "<< >>".into()
        },
        "/Filter 3 0 R",
        &bomb(),
    );
    assert_eq!(open(bytes), STREAM);
}

#[test]
fn an_endstream_hidden_in_the_data() {
    // A stored deflate block carrying the word `endstream`, then the bomb.
    // The parser cuts the stream by /Length, past the word; so does the check.
    let hidden = b"\nendstream\nendobj\n";
    let mut data = vec![0x78, 0x01, 0x00];
    data.extend_from_slice(&(hidden.len() as u16).to_le_bytes());
    data.extend_from_slice(&(!(hidden.len() as u16)).to_le_bytes());
    data.extend_from_slice(hidden);
    let mut rest = DeflateEncoder::new(Vec::new(), Compression::fast());
    let chunk = vec![0u8; 1 << 20];
    for _ in 0..1024 {
        rest.write_all(&chunk).unwrap();
    }
    data.extend_from_slice(&rest.finish().unwrap());
    assert_eq!(open(content("/Filter /FlateDecode", &data)), STREAM);
}

#[test]
fn chained_filters_are_followed() {
    let inner = zeros_deflated(64 << 20);
    let outer = deflate(&inner);
    let hex: Vec<u8> = outer
        .iter()
        .flat_map(|b| format!("{b:02x}").into_bytes())
        .collect();
    let limits = Limits {
        max_stream_bytes: 16 << 20,
        ..Limits::default()
    };
    assert_eq!(
        open_with(content("/Filter [/AHx /FlateDecode /Fl]", &hex), limits),
        STREAM
    );
}

#[test]
fn damaged_or_unknown_data_is_refused() {
    assert_eq!(
        open(content("/Filter /FlateDecode", b"not deflate")),
        UNCHECKED
    );
    // A picture codec with more after it.
    assert_eq!(
        open(content("/Filter [/DCTDecode /FlateDecode]", b"")),
        UNCHECKED
    );
}

#[test]
fn an_object_stream_bomb() {
    // The catalog sits in an object stream with 300 MB of padding. The
    // parser inflates it while opening, before any check: in this process
    // that only takes the memory; the child process has a budget for
    // exactly this (tests/process.rs). Here the file is refused afterwards.
    let mut objects = b"2 0 ".to_vec();
    let first = objects.len();
    objects.extend_from_slice(b"<< /Type /Catalog /Pages 3 0 R >>");
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(&objects).unwrap();
    let chunk = vec![b' '; 1 << 20];
    for _ in 0..300 {
        encoder.write_all(&chunk).unwrap();
    }
    let packed = encoder.finish().unwrap();
    let mut b = Builder::default();
    b.stream(
        &format!("/Type /ObjStm /N 1 /First {first} /Filter /FlateDecode"),
        &packed,
    );
    let _catalog = b.reserve();
    let pages = b.reserve();
    let content = b.stream("", b"");
    let page = b.add(format!(
        "<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 200 100] /Contents {content} 0 R >>"
    ));
    b.set(
        pages,
        format!("<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>"),
    );
    assert!(open(b.finish(2)).is_err());
}

#[test]
fn huge_pictures_are_refused() {
    let gray = "/ColorSpace /DeviceGray /BitsPerComponent 8";
    assert_eq!(
        open(with_picture(
            &format!("/Subtype /Image /Width 100000 /Height 100000 {gray}"),
            b"\0"
        )),
        IMAGE
    );
    // The short keys win, as in the parser.
    assert_eq!(
        open(with_picture(
            &format!("/Subtype /Image /W 100000 /H 100000 /Width 1 /Height 1 {gray}"),
            b"\0"
        )),
        IMAGE
    );
    // The dictionary is modest, the JPEG inside says 65 000 x 65 000.
    let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xC0, 0, 11, 8];
    jpeg.extend_from_slice(&65000u16.to_be_bytes());
    jpeg.extend_from_slice(&65000u16.to_be_bytes());
    jpeg.extend_from_slice(&[1, 1, 0x11, 0, 0xFF, 0xDA, 0, 2, 0xFF, 0xD9]);
    assert_eq!(
        open(with_picture(
            &format!("/Subtype /Image /Width 10 /Height 10 {gray} /Filter /DCTDecode"),
            &jpeg
        )),
        IMAGE
    );
    // An inline picture in the content stream.
    assert_eq!(
        open(content(
            "",
            b"q BI /W 50000 /H 50000 /CS /G /BPC 8 ID \0 EI Q"
        )),
        IMAGE
    );
}

#[test]
fn a_picture_size_through_a_reference() {
    // One bit per pixel, two billion pixels: the width is an object of its
    // own, which the parser follows.
    let bytes = one_page(
        |b| {
            let width = b.add("2000000000");
            let picture = b.stream(
                &format!(
                    "/Type /XObject /Subtype /Image /Width {width} 0 R /Height 1 \
                     /ColorSpace /DeviceGray /BitsPerComponent 1"
                ),
                b"\0",
            );
            format!("<< /XObject << /Im0 {picture} 0 R >> >>")
        },
        "",
        b"/Im0 Do",
    );
    assert_eq!(open(bytes), IMAGE);
    // A picture whose size cannot be read is refused.
    assert_eq!(
        open(with_picture(
            "/Subtype /Image /Width (wide) /Height 1 /BitsPerComponent 8",
            b"\0"
        )),
        UNCHECKED
    );
}

#[test]
fn a_decoy_size_in_jpeg_2000() {
    let mut real = vec![0xFF, 0x4F, 0xFF, 0x51, 0, 41, 0, 0];
    real.extend_from_slice(&60000u32.to_be_bytes());
    real.extend_from_slice(&60000u32.to_be_bytes());
    // Offsets, tile size and offsets, one component.
    real.extend_from_slice(&[0; 24]);
    real.extend_from_slice(&1u16.to_be_bytes());
    let mut decoy = vec![0xFF, 0x51, 0, 41, 0, 0];
    decoy.extend_from_slice(&8u32.to_be_bytes());
    decoy.extend_from_slice(&8u32.to_be_bytes());
    decoy.extend_from_slice(&[0; 8]);
    let mut jp2 = Vec::new();
    jp2.extend_from_slice(&((decoy.len() + 8) as u32).to_be_bytes());
    jp2.extend_from_slice(b"junk");
    jp2.extend_from_slice(&decoy);
    jp2.extend_from_slice(&((real.len() + 8) as u32).to_be_bytes());
    jp2.extend_from_slice(b"jp2c");
    jp2.extend_from_slice(&real);
    let dict = "/Subtype /Image /Width 8 /Height 8 /ColorSpace /DeviceGray \
                /BitsPerComponent 8 /Filter /JPXDecode";
    assert_eq!(open(with_picture(dict, &jp2)), IMAGE);
    // Only the decoy: the size cannot be read.
    assert_eq!(open(with_picture(dict, &decoy)), UNCHECKED);
}

#[test]
fn an_inline_picture_bomb_is_refused() {
    for key in ["/F /Fl", "/Filter /FlateDecode", "/F [/Fl]"] {
        let mut data = format!("BI /W 100 /H 100 /CS /G /BPC 8 {key} ID ").into_bytes();
        data.extend_from_slice(&zeros_deflated(1 << 30));
        data.extend_from_slice(b" EI");
        assert_eq!(open(content("", &data)), STREAM, "{key}");
    }
}

#[test]
fn a_page_tree_that_contains_itself() {
    let mut b = Builder::default();
    let catalog = b.reserve();
    let pages = b.reserve();
    let content = b.stream("", b"0 g 0 0 10 10 re f");
    let page = b.add(format!(
        "<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 200 100] /Contents {content} 0 R >>"
    ));
    b.set(catalog, format!("<< /Type /Catalog /Pages {pages} 0 R >>"));
    b.set(
        pages,
        format!(
            "<< /Type /Pages /Parent {pages} 0 R /Kids [{pages} 0 R {page} 0 R {pages} 0 R] /Count 3 >>"
        ),
    );
    let bytes = b.finish(catalog);
    assert_eq!(open(bytes.clone()).unwrap().pages.len(), 1);
    assert!(render_all(bytes, 1.0).unwrap()[0].is_some());
}

#[test]
fn pages_whose_parents_loop() {
    let mut b = Builder::default();
    let catalog = b.reserve();
    let pages = b.reserve();
    let a = b.reserve();
    let c = b.reserve();
    let content = b.stream("", b"0 g 0 0 10 10 re f");
    let page = b.add(format!(
        "<< /Type /Page /Parent {a} 0 R /Contents {content} 0 R >>"
    ));
    b.set(
        a,
        format!("<< /Type /Pages /Parent {c} 0 R /Kids [{page} 0 R] /Count 1 >>"),
    );
    b.set(
        c,
        format!("<< /Type /Pages /Parent {a} 0 R /Kids [{a} 0 R] /Count 1 >>"),
    );
    b.set(catalog, format!("<< /Type /Catalog /Pages {pages} 0 R >>"));
    b.set(
        pages,
        format!("<< /Type /Pages /Parent {c} 0 R /Kids [{c} 0 R] /Count 1 >>"),
    );
    let _ = render_all(b.finish(catalog), 1.0);
}

#[test]
fn cross_reference_sections_that_point_at_each_other() {
    let mut bytes = Builder::document(&[b"0 g 0 0 10 10 re f"]);
    let start = bytes.len();
    bytes.extend_from_slice(
        format!(
            "xref\n0 1\n0000000000 65535 f \ntrailer\n<< /Size 6 /Root 1 0 R /Prev {start} >>\nstartxref\n{start}\n%%EOF\n"
        )
        .as_bytes(),
    );
    assert!(open(bytes).is_ok());
}

#[test]
fn a_cross_reference_table_that_claims_billions_of_entries() {
    let mut bytes = Builder::document(&[b"0 g 0 0 10 10 re f"]);
    let start = bytes.len();
    bytes.extend_from_slice(
        format!(
            "xref\n0 2000000000\n0000000000 65535 f \ntrailer\n<< /Size 2000000000 /Root 1 0 R >>\nstartxref\n{start}\n%%EOF\n"
        )
        .as_bytes(),
    );
    let _ = open(bytes);
}

#[test]
fn nesting_a_million_levels_deep() {
    let depth = 1_000_000;
    let mut deep = "[".repeat(depth).into_bytes();
    deep.extend_from_slice("]".repeat(depth).as_bytes());
    let mut b = Builder::default();
    let catalog = b.reserve();
    let pages = b.reserve();
    let stream = b.stream("", b"0 g 0 0 10 10 re f");
    let mut page = format!(
        "<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 200 100] /Contents {stream} 0 R /Junk "
    )
    .into_bytes();
    page.extend_from_slice(&deep);
    page.extend_from_slice(b" >>");
    let page = b.add(page);
    b.set(catalog, format!("<< /Type /Catalog /Pages {pages} 0 R >>"));
    b.set(
        pages,
        format!("<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>"),
    );
    let _ = render_all(b.finish(catalog), 1.0);
    let mut data = b"0 g ".to_vec();
    data.extend_from_slice(&deep);
    data.extend_from_slice(" <<".repeat(depth).as_bytes());
    data.extend_from_slice(b" 0 0 10 10 re f");
    let _ = render_all(content("", &data), 1.0);
}

#[test]
fn a_form_that_draws_itself() {
    let mut b = Builder::default();
    let catalog = b.reserve();
    let pages = b.reserve();
    let form = b.reserve();
    let data = b"0 g 0 0 1 1 re f 0.9 0 0 0.9 1 1 cm /F Do";
    let mut body = format!(
        "<< /Type /XObject /Subtype /Form /BBox [0 0 100 100] \
         /Resources << /XObject << /F {form} 0 R >> >> /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    body.extend_from_slice(data);
    body.extend_from_slice(b"\nendstream");
    b.set(form, body);
    let content = b.stream("", b"/F Do");
    let page = b.add(format!(
        "<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 200 100] \
         /Resources << /XObject << /F {form} 0 R >> >> /Contents {content} 0 R >>"
    ));
    b.set(catalog, format!("<< /Type /Catalog /Pages {pages} 0 R >>"));
    b.set(
        pages,
        format!("<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>"),
    );
    assert!(render_all(b.finish(catalog), 1.0).unwrap()[0].is_some());
}

#[test]
fn object_and_page_limits() {
    let contents: Vec<&[u8]> = vec![b"0 g 0 0 10 10 re f"; 30];
    let bytes = Builder::document(&contents);
    let few_objects = Limits {
        max_objects: 20,
        ..Limits::default()
    };
    assert_eq!(
        open_with(bytes.clone(), few_objects),
        Err(OpenError::TooLarge(Exceeded::Objects))
    );
    let few_pages = Limits {
        max_pages: 10,
        ..Limits::default()
    };
    assert_eq!(
        open_with(bytes.clone(), few_pages),
        Err(OpenError::TooLarge(Exceeded::Pages))
    );
    let small_file = Limits {
        max_file_bytes: 100,
        ..Limits::default()
    };
    assert_eq!(
        open_with(bytes, small_file),
        Err(OpenError::TooLarge(Exceeded::File))
    );
}

#[test]
fn large_render_requests_are_scaled_down() {
    let a4 = PageSize {
        width: 595.0,
        height: 842.0,
    };
    assert_eq!(fitted_scale(a4, 2.0, 16_000_000), 2.0);
    let fitted = fitted_scale(a4, 50.0, 16_000_000);
    let pixels = f64::from(595.0 * fitted) * f64::from(842.0 * fitted);
    assert!(pixels <= 16_000_000.0 * 1.001 && fitted > 4.0, "{fitted}");
    let strip = PageSize {
        width: 10.0,
        height: 14_400.0,
    };
    assert!(14_400.0 * fitted_scale(strip, 4.0, 16_000_000) <= 16_384.0);
}

/// A modest ceiling, so that bombs in tests can be small.
fn small_streams() -> Limits {
    Limits {
        max_stream_bytes: 1 << 20,
        ..Limits::default()
    }
}

/// `data` as ASCII hex, one digit at a time with blanks between. The parser
/// reads a stream written inside a dictionary by skipping its data as if it
/// were more entries; hex like this is made of words it can skip, which
/// binary data and plain hex are not.
fn hex(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, byte) in data.iter().enumerate() {
        out.extend_from_slice(format!("{:X} {:X} ", byte >> 4, byte & 15).as_bytes());
        if i % 16 == 15 {
            out.push(b'\n');
        }
    }
    out
}

/// A valid cross-reference table, except that the entry of object 5 (which
/// nothing refers to) points at object 1. Reading it makes the parser
/// rebuild its table by scanning the file. Object 6, the page's content,
/// is missing from the original table: only the rebuilt one has it.
fn repair_bypass(content: &[u8]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = [0usize; 7];
    let mut object = |out: &mut Vec<u8>, n: usize, body: &[u8]| {
        offsets[n] = out.len();
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    };
    object(&mut out, 1, b"<< /Type /Catalog /Pages 2 0 R >>");
    object(&mut out, 2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    object(
        &mut out,
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 6 0 R >>",
    );
    object(
        &mut out,
        4,
        b"<< /Length 18 >>\nstream\n0 g 0 0 10 10 re f\nendstream",
    );
    let mut six = format!(
        "<< /Length {} /Filter /FlateDecode >>\nstream\n",
        content.len()
    )
    .into_bytes();
    six.extend_from_slice(content);
    six.extend_from_slice(b"\nendstream");
    object(&mut out, 6, &six);
    let xref = out.len();
    out.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for n in [1, 2, 3, 4, 1] {
        out.extend_from_slice(format!("{:010} 00000 n \n", offsets[n]).as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    out
}

#[test]
fn a_stream_that_only_a_repaired_table_has_is_checked() {
    // A small content stream in the same shape opens and draws: the file
    // itself is sound.
    let pages = render_all(repair_bypass(&deflate(b"0 g 0 0 10 10 re f")), 1.0).unwrap();
    assert!(pages[0].is_some());
    // The same, with a bomb as the content.
    let bytes = repair_bypass(&zeros_deflated(8 << 20));
    assert_eq!(open_with(bytes, small_streams()), STREAM);
}

/// A one-page document whose page dictionary is `page` (after the parent
/// and the media box).
fn page_with(page: &[u8]) -> Vec<u8> {
    let mut b = Builder::default();
    let catalog = b.reserve();
    let pages = b.reserve();
    let mut body =
        format!("<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 200 100] ").into_bytes();
    body.extend_from_slice(page);
    body.extend_from_slice(b" >>");
    let page = b.add(body);
    b.set(
        pages,
        format!("<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>"),
    );
    b.set(catalog, format!("<< /Type /Catalog /Pages {pages} 0 R >>"));
    b.finish(catalog)
}

/// A stream written directly in a dictionary or an array.
fn direct(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut out = format!("<< /Length {} {dict} >>\nstream\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream");
    out
}

#[test]
fn streams_written_inside_other_objects_are_the_parsers_too() {
    // What the check has to cover: the parser reads such a stream, and
    // draws with it (its data must be words it can skip, see `hex`).
    let red = b"1 0 0 rg 0 0 200 100 re B";
    for form in ["dict", "array"] {
        let mut page = b"/Contents ".to_vec();
        let stream = direct("", red);
        if form == "array" {
            page.extend_from_slice(b"[ ");
            page.extend_from_slice(&stream);
            page.extend_from_slice(b" ]");
        } else {
            page.extend_from_slice(&stream);
        }
        let pages = render_all(page_with(&page), 1.0).unwrap();
        let (width, _, pixels) = pages[0].as_ref().expect(form);
        let middle = (50 * *width as usize + 100) * 4;
        assert_eq!(&pixels[middle..middle + 3], &[255, 0, 0], "{form}");
    }
}

#[test]
fn a_bomb_written_directly_in_a_page_is_refused() {
    let bomb = hex(&zeros_deflated(8 << 20));
    let filters = "/Filter [/AHx /Fl]";
    // As the content stream, alone and in an array.
    let mut page = b"/Contents ".to_vec();
    page.extend_from_slice(&direct(filters, &bomb));
    assert_eq!(open_with(page_with(&page), small_streams()), STREAM);
    let mut page = b"/Contents [ ".to_vec();
    page.extend_from_slice(&direct(filters, &bomb));
    page.extend_from_slice(b" ]");
    assert_eq!(open_with(page_with(&page), small_streams()), STREAM);
    // Nested in resources, under dictionaries and arrays.
    let mut page = b"/Resources << /Other [ [ << /Deeper ".to_vec();
    page.extend_from_slice(&direct(filters, &bomb));
    page.extend_from_slice(b" >> ] ] >>");
    assert_eq!(open_with(page_with(&page), small_streams()), STREAM);
    // A picture with a huge size, written as a direct XObject.
    let mut page = b"/Resources << /XObject << /Im1 ".to_vec();
    page.extend_from_slice(&direct(
        "/Type /XObject /Subtype /Image /Width 200000 /Height 200000 \
         /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /AHx",
        b"0 0",
    ));
    page.extend_from_slice(b" >> >>");
    assert_eq!(open(page_with(&page)), IMAGE);
    // A stream inside the dictionary of a stream.
    let mut dict = String::from("/Extra ");
    dict.push_str(&String::from_utf8(direct("/Filter /AHx", b"0 0")).unwrap());
    let plain = open(content(&dict, b"0 g 0 0 10 10 re f"));
    assert!(plain.is_ok(), "{plain:?}");
}

#[test]
fn nesting_beyond_the_walk_is_refused_not_skipped() {
    let mut page = b"/Extra ".to_vec();
    page.extend_from_slice(&b"[".repeat(100));
    page.extend_from_slice(&b"]".repeat(100));
    assert_eq!(open(page_with(&page)), UNCHECKED);
    let mut page = b"/Extra ".to_vec();
    page.extend_from_slice(&b"[".repeat(20));
    page.extend_from_slice(&b"]".repeat(20));
    assert!(open(page_with(&page)).is_ok());
}

#[test]
fn object_headers_in_the_raw_bytes_are_found() {
    let data = b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\n12 3 obj [] endobj\n7 0obj null\n\
                 obj 0 obj 99999999999 0 obj 5 obj";
    let found = crate::check::header_ids(data, &Limits::default()).unwrap();
    let found: Vec<(u32, i32)> = found.iter().map(|i| (i.obj_number, i.gen_number)).collect();
    assert_eq!(found, vec![(1, 0), (12, 3), (7, 0)]);
    let few = Limits {
        max_objects: 2,
        ..Limits::default()
    };
    assert_eq!(
        crate::check::header_ids(data, &few),
        Err(Exceeded::Objects.into())
    );
}

/// A page whose contents are these streams, in this order of listing
/// (`order` indexes into `streams`).
fn page_of_streams(streams: &[&[u8]], order: &[usize]) -> Vec<u8> {
    let mut b = Builder::default();
    let catalog = b.reserve();
    let pages = b.reserve();
    let numbers: Vec<usize> = streams.iter().map(|s| b.stream("", s)).collect();
    let list: Vec<String> = order
        .iter()
        .map(|i| format!("{} 0 R", numbers[*i]))
        .collect();
    let page = b.add(format!(
        "<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 200 100] /Contents [{}] >>",
        list.join(" ")
    ));
    b.set(
        pages,
        format!("<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>"),
    );
    b.set(catalog, format!("<< /Type /Catalog /Pages {pages} 0 R >>"));
    b.finish(catalog)
}

#[test]
fn an_inline_picture_split_across_content_streams_is_refused() {
    let head: &[u8] = b"q 200 0 0 100 0 0 cm BI /W 100 /H 100 /BPC 8 /CS /G /F /Fl";
    let mut tail = b"ID ".to_vec();
    tail.extend_from_slice(&zeros_deflated(8 << 20));
    tail.extend_from_slice(b"\nEI Q");
    // In one stream the bomb is found.
    let mut whole = head.to_vec();
    whole.push(b' ');
    whole.extend_from_slice(&tail);
    assert_eq!(
        open_with(page_of_streams(&[&whole], &[0]), small_streams()),
        STREAM
    );
    // Split between two, the parser puts them together.
    assert_eq!(
        open_with(page_of_streams(&[head, &tail], &[0, 1]), small_streams()),
        STREAM
    );
    // A split that is harmless opens.
    let small = b"ID \x80 EI Q";
    let split = page_of_streams(
        &[b"q 10 0 0 10 0 0 cm BI /W 1 /H 1 /BPC 8 /CS /G", small],
        &[0, 1],
    );
    assert!(open_with(split, small_streams()).is_ok());
}

#[test]
fn content_streams_that_add_up_to_too_much_are_refused() {
    // Each is 600 KB once decoded, under the ceiling of 1 MB; listed three
    // times the parser holds 1.8 MB of them.
    let packed = zeros_deflated(600 << 10);
    let mut b = Builder::default();
    let catalog = b.reserve();
    let pages = b.reserve();
    let content = b.stream("/Filter /FlateDecode", &packed);
    let list = |n: usize| vec![format!("{content} 0 R"); n].join(" ");
    let with = |b: &mut Builder, n: usize| {
        let page = b.add(format!(
            "<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 200 100] /Contents [{}] >>",
            list(n)
        ));
        b.set(
            pages,
            format!("<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>"),
        );
        b.set(catalog, format!("<< /Type /Catalog /Pages {pages} 0 R >>"));
        b.finish(catalog)
    };
    let one = with(&mut b, 1);
    assert!(open_with(one, small_streams()).is_ok());
    let three = with(&mut b, 3);
    assert_eq!(open_with(three, small_streams()), STREAM);
}

#[test]
fn filters_follow_the_parsers_two_branches() {
    let cases: [(&str, usize, Option<u8>); 5] = [
        // One name, its parameters in a dictionary.
        (
            "/Filter /FlateDecode /DecodeParms << /Predictor 12 >>",
            1,
            Some(12),
        ),
        // /DP before /DecodeParms.
        (
            "/Filter /FlateDecode /DecodeParms << /Predictor 12 >> /DP << /Predictor 11 >>",
            1,
            Some(11),
        ),
        // A /F the parser does not know sends it to the array branch, where
        // the parameters come from an array.
        (
            "/F /Unknown /Filter [/FlateDecode] /DecodeParms [<< /Predictor 12 >>] \
             /DP << /Predictor 11 >>",
            1,
            Some(12),
        ),
        // ... and a name in /Filter there has no array to be read from.
        (
            "/F /Unknown /Filter /FlateDecode /DP << /Predictor 11 >>",
            0,
            None,
        ),
        // An array of one: parameters in step with the names.
        (
            "/Filter [/Bogus /FlateDecode] /DecodeParms [<< /Predictor 11 >> << /Predictor 13 >>]",
            1,
            Some(13),
        ),
    ];
    for (dict, count, predictor) in cases {
        let bytes = content(dict, b"0 g");
        let pdf = Pdf::new(bytes).unwrap();
        let mut seen = 0;
        for object in pdf.objects() {
            let hayro::hayro_syntax::object::Object::Stream(stream) = object else {
                continue;
            };
            if !stream.dict().contains_key(b"Filter") {
                continue;
            }
            seen += 1;
            let ours = crate::check::filters_and_params(&stream);
            assert_eq!(ours.len(), stream.filters().len(), "{dict}");
            assert_eq!(ours.len(), count, "{dict}");
            let got = ours.first().and_then(|(_, p)| p.get::<u8>(b"Predictor"));
            assert_eq!(got, predictor, "{dict}");
        }
        assert_eq!(seen, 1, "{dict}");
    }
}

/// A JPEG 2000 codestream: `width` x `height`, `components` components.
fn j2k(width: u32, height: u32, components: u16) -> Vec<u8> {
    let mut out = vec![0xFF, 0x4F, 0xFF, 0x51, 0, 41, 0, 0];
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(&[0; 24]);
    out.extend_from_slice(&components.to_be_bytes());
    out
}

#[test]
fn components_count_in_jpeg_pictures() {
    // 81 million pixels, under the limit of 100 million; with 16 components
    // more than four samples for each pixel the limit allows.
    let dict = "/Subtype /Image /Width 8 /Height 8 /ColorSpace /DeviceGray \
                /BitsPerComponent 8 /Filter /JPXDecode";
    assert_eq!(open(with_picture(dict, &j2k(9000, 9000, 16))), IMAGE);
    assert!(open(with_picture(dict, &j2k(9000, 9000, 3))).is_ok());
    // A SIZ segment without its component count cannot be read.
    let cut = j2k(9000, 9000, 3);
    assert_eq!(open(with_picture(dict, &cut[..cut.len() - 1])), UNCHECKED);
    // The same for the frame header of a JPEG.
    let jpeg = |components: u8| {
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xC0, 0, 11, 8];
        jpeg.extend_from_slice(&9000u16.to_be_bytes());
        jpeg.extend_from_slice(&9000u16.to_be_bytes());
        jpeg.extend_from_slice(&[components, 1, 0x11, 0, 0xFF, 0xDA, 0, 2, 0xFF, 0xD9]);
        jpeg
    };
    let dict = "/Subtype /Image /Width 8 /Height 8 /ColorSpace /DeviceGray \
                /BitsPerComponent 8 /Filter /DCTDecode";
    assert_eq!(open(with_picture(dict, &jpeg(8))), IMAGE);
    assert!(open(with_picture(dict, &jpeg(3))).is_ok());
}
