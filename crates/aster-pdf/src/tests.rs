//! Opening and rendering, and files built to hurt: streams that inflate to
//! gigabytes, pictures with billions of pixels, loops in the page tree and in
//! the cross-reference chain, nesting a million levels deep. Each must end in
//! an error or a page, without running out of stack or memory.

use std::io::Write;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use flate2::Compression;
use flate2::write::ZlibEncoder;

use super::*;

/// Writes a PDF with a correct cross-reference table. Object `n` is the
/// `n`-th one added, counting from 1.
#[derive(Default)]
pub(crate) struct Builder {
    objects: Vec<Vec<u8>>,
}

impl Builder {
    pub(crate) fn add(&mut self, body: impl AsRef<[u8]>) -> usize {
        self.objects.push(body.as_ref().to_vec());
        self.objects.len()
    }

    pub(crate) fn stream(&mut self, dict: &str, data: &[u8]) -> usize {
        let mut body = format!("<< /Length {} {dict} >>\nstream\n", data.len()).into_bytes();
        body.extend_from_slice(data);
        body.extend_from_slice(b"\nendstream");
        self.add(body)
    }

    /// Reserve a number to fill in later (for loops).
    pub(crate) fn reserve(&mut self) -> usize {
        self.add(b"null")
    }

    pub(crate) fn set(&mut self, number: usize, body: impl AsRef<[u8]>) {
        self.objects[number - 1] = body.as_ref().to_vec();
    }

    pub(crate) fn finish(&self, root: usize) -> Vec<u8> {
        self.finish_with(root, "")
    }

    pub(crate) fn finish_with(&self, root: usize, trailer: &str) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
        let mut offsets = Vec::new();
        for (index, body) in self.objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", self.objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root {root} 0 R {trailer} >>\nstartxref\n{xref}\n%%EOF\n",
                self.objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    /// A catalog, a page tree and one page per content stream.
    pub(crate) fn document(contents: &[&[u8]]) -> Vec<u8> {
        let mut b = Builder::default();
        let catalog = b.reserve();
        let pages = b.reserve();
        let mut kids = Vec::new();
        for content in contents {
            let stream = b.stream("", content);
            kids.push(b.add(format!(
                "<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 200 100] /Contents {stream} 0 R >>"
            )));
        }
        let kids: Vec<String> = kids.iter().map(|k| format!("{k} 0 R")).collect();
        b.set(catalog, format!("<< /Type /Catalog /Pages {pages} 0 R >>"));
        b.set(
            pages,
            format!(
                "<< /Type /Pages /Kids [{}] /Count {} >>",
                kids.join(" "),
                contents.len()
            ),
        );
        b.finish(catalog)
    }
}

pub(crate) fn deflate(data: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

/// `bytes` zeros, deflated, without holding them all.
fn zeros_deflated(bytes: usize) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    let chunk = vec![0u8; 1 << 20];
    let mut left = bytes;
    while left > 0 {
        let n = left.min(chunk.len());
        encoder.write_all(&chunk[..n]).unwrap();
        left -= n;
    }
    encoder.finish().unwrap()
}

/// Open on a thread like the worker's, with a time limit.
fn open_with(bytes: Vec<u8>, password: &str, limits: Limits) -> Result<Info, OpenError> {
    let (send, receive) = mpsc::channel();
    let password = password.to_string();
    std::thread::Builder::new()
        .stack_size(STACK_BYTES)
        .spawn(move || {
            let result = Document::open(bytes, &password, limits).map(|d| d.info().clone());
            let _ = send.send(result);
        })
        .unwrap();
    receive
        .recv_timeout(Duration::from_secs(60))
        .expect("open took more than a minute")
}

fn open(bytes: Vec<u8>) -> Result<Info, OpenError> {
    open_with(bytes, "", Limits::default())
}

/// Open and render every page through a worker, collecting what it reports.
fn render_all(bytes: Vec<u8>, scale: f32) -> Vec<Event> {
    let (send, receive) = mpsc::channel();
    let worker = Worker::spawn(bytes, String::new(), Limits::default(), move |event| {
        let _ = send.send(event);
    });
    let mut events = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut pages = None;
    while let Ok(event) = receive.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        match &event {
            Event::Opened(info) => {
                pages = Some(info.pages.len());
                worker.want((0..info.pages.len()).map(|p| (p, scale)).collect());
            }
            Event::Failed(_) => {
                events.push(event);
                return events;
            }
            _ => {}
        }
        events.push(event);
        let done = events
            .iter()
            .filter(|e| matches!(e, Event::Page(_) | Event::PageFailed(_)))
            .count();
        if pages == Some(done) {
            return events;
        }
    }
    panic!("the worker did not finish within a minute: {events:?}");
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
    let events = render_all(bytes, 2.0);
    let Some(Event::Page(page)) = events.last() else {
        panic!("{events:?}");
    };
    assert_eq!((page.width, page.height), (400, 200));
    // Inside the red square (40..160 x 40..160 from the bottom), and outside.
    let pixel = |x: u32, y: u32| {
        let i = ((y * page.width + x) * 4) as usize;
        page.pixels[i..i + 4].to_vec()
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
        let result = open(bytes[..end].to_vec());
        if let Ok(info) = result {
            assert!(!info.pages.is_empty());
        }
    }
}

#[test]
fn a_stream_that_inflates_to_gigabytes_is_refused() {
    // 2 GB of zeros in a content stream: a few megabytes on disk.
    let bomb = zeros_deflated(2 << 30);
    assert!(bomb.len() < 32 << 20);
    let mut b = Builder::default();
    let catalog = b.reserve();
    let pages = b.reserve();
    let content = b.stream("/Filter /FlateDecode", &bomb);
    let page = b.add(format!(
        "<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 200 100] /Contents {content} 0 R >>"
    ));
    b.set(catalog, format!("<< /Type /Catalog /Pages {pages} 0 R >>"));
    b.set(
        pages,
        format!("<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>"),
    );
    assert_eq!(
        open(b.finish(catalog)),
        Err(OpenError::TooLarge(Exceeded::Stream))
    );
}

#[test]
fn chained_filters_are_followed() {
    // Deflate inside deflate inside hex: each layer is small, the end is not.
    let inner = zeros_deflated(64 << 20);
    let outer = deflate(&inner);
    let hex: Vec<u8> = outer
        .iter()
        .flat_map(|b| format!("{b:02x}").into_bytes())
        .collect();
    let mut b = Builder::default();
    let catalog = b.reserve();
    let pages = b.reserve();
    let content = b.stream("/Filter [/AHx /FlateDecode /Fl]", &hex);
    let page = b.add(format!(
        "<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 200 100] /Contents {content} 0 R >>"
    ));
    b.set(catalog, format!("<< /Type /Catalog /Pages {pages} 0 R >>"));
    b.set(
        pages,
        format!("<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>"),
    );
    let limits = Limits {
        max_stream_bytes: 16 << 20,
        ..Limits::default()
    };
    assert_eq!(
        open_with(b.finish(catalog), "", limits),
        Err(OpenError::TooLarge(Exceeded::Stream))
    );
}

#[test]
fn an_object_stream_bomb_is_refused_before_parsing() {
    // The catalog sits in an object stream that also carries 1 GB of
    // padding; the parser would inflate it while opening.
    let mut objects = b"2 0 ".to_vec();
    let first = objects.len();
    objects.extend_from_slice(b"<< /Type /Catalog /Pages 3 0 R >>");
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(&objects).unwrap();
    let chunk = vec![b' '; 1 << 20];
    for _ in 0..1024 {
        encoder.write_all(&chunk).unwrap();
    }
    let packed = encoder.finish().unwrap();
    let mut b = Builder::default();
    let objstm = b.stream(
        &format!("/Type /ObjStm /N 1 /First {first} /Filter /FlateDecode"),
        &packed,
    );
    let _catalog_placeholder = b.reserve();
    let pages = b.reserve();
    let content = b.stream("", b"");
    let page = b.add(format!(
        "<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 200 100] /Contents {content} 0 R >>"
    ));
    b.set(
        pages,
        format!("<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>"),
    );
    assert_eq!(objstm, 1);
    assert_eq!(
        open(b.finish(2)),
        Err(OpenError::TooLarge(Exceeded::Stream))
    );
}

#[test]
fn huge_pictures_are_refused() {
    let image = |dict: &str, data: &[u8]| {
        let mut b = Builder::default();
        let catalog = b.reserve();
        let pages = b.reserve();
        let picture = b.stream(&format!("/Type /XObject /Subtype /Image {dict}"), data);
        let content = b.stream("", b"q 100 0 0 100 0 0 cm /Im0 Do Q");
        let page = b.add(format!(
            "<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 200 100] \
             /Resources << /XObject << /Im0 {picture} 0 R >> >> /Contents {content} 0 R >>"
        ));
        b.set(catalog, format!("<< /Type /Catalog /Pages {pages} 0 R >>"));
        b.set(
            pages,
            format!("<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>"),
        );
        open(b.finish(catalog))
    };
    let refused = Err(OpenError::TooLarge(Exceeded::Image));
    // The dictionary says 100 000 x 100 000.
    assert_eq!(
        image(
            "/Width 100000 /Height 100000 /ColorSpace /DeviceGray /BitsPerComponent 8",
            b"\0"
        ),
        refused
    );
    // The dictionary is modest, the JPEG inside says 65 000 x 65 000.
    let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xC0, 0, 11, 8];
    jpeg.extend_from_slice(&65000u16.to_be_bytes());
    jpeg.extend_from_slice(&65000u16.to_be_bytes());
    jpeg.extend_from_slice(&[1, 1, 0x11, 0, 0xFF, 0xD9]);
    assert_eq!(
        image(
            "/Width 10 /Height 10 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /DCTDecode",
            &jpeg
        ),
        refused
    );
    // Width through a reference to an integer object is still read: build it
    // by hand so the integer is an object of its own.
    let mut b = Builder::default();
    let catalog = b.reserve();
    let pages = b.reserve();
    let width = b.add(b"200000");
    let picture = b.stream(
        &format!("/Type /XObject /Subtype /Image /Width {width} 0 R /Height 100000 /ColorSpace /DeviceGray /BitsPerComponent 8"),
        b"\0",
    );
    let content = b.stream("", b"/Im0 Do");
    let page = b.add(format!(
        "<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 200 100] \
         /Resources << /XObject << /Im0 {picture} 0 R >> >> /Contents {content} 0 R >>"
    ));
    b.set(catalog, format!("<< /Type /Catalog /Pages {pages} 0 R >>"));
    b.set(
        pages,
        format!("<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>"),
    );
    assert_eq!(open(b.finish(catalog)), refused);
    // An inline picture in the content stream.
    let inline = Builder::document(&[b"q BI /W 50000 /H 50000 /CS /G /BPC 8 ID \0 EI Q"]);
    assert_eq!(open(inline), refused);
}

#[test]
fn an_inline_picture_bomb_is_refused() {
    let mut content = b"BI /W 100 /H 100 /CS /G /BPC 8 /F /Fl ID ".to_vec();
    content.extend_from_slice(&zeros_deflated(1 << 30));
    content.extend_from_slice(b" EI");
    assert_eq!(
        open(Builder::document(&[&content])),
        Err(OpenError::TooLarge(Exceeded::Stream))
    );
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
        format!("<< /Type /Pages /Parent {pages} 0 R /Kids [{pages} 0 R {page} 0 R {pages} 0 R] /Count 3 >>"),
    );
    let bytes = b.finish(catalog);
    let info = open(bytes.clone()).unwrap();
    assert_eq!(info.pages.len(), 1);
    assert!(
        render_all(bytes, 1.0)
            .iter()
            .any(|e| matches!(e, Event::Page(_)))
    );
}

#[test]
fn pages_whose_parents_loop() {
    // Attributes are inherited through /Parent; here the chain is a loop and
    // nothing defines a media box.
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
    let bytes = b.finish(catalog);
    if open(bytes.clone()).is_ok() {
        render_all(bytes, 1.0);
    }
}

#[test]
fn cross_reference_sections_that_point_at_each_other() {
    let mut bytes = Builder::document(&[b"0 g 0 0 10 10 re f"]);
    // Append a second section whose /Prev is itself, and make it the last.
    let start = bytes.len();
    bytes.extend_from_slice(
        format!("xref\n0 1\n0000000000 65535 f \ntrailer\n<< /Size 6 /Root 1 0 R /Prev {start} >>\nstartxref\n{start}\n%%EOF\n")
            .as_bytes(),
    );
    let result = open(bytes.clone());
    assert!(result.is_ok(), "{result:?}");
    // And two sections that point at each other.
    let second = bytes.len();
    let text = format!(
        "xref\n0 1\n0000000000 65535 f \ntrailer\n<< /Size 6 /Root 1 0 R /Prev {start} >>\nstartxref\n{second}\n%%EOF\n"
    );
    bytes.extend_from_slice(text.as_bytes());
    let patched = String::from_utf8_lossy(&bytes).replace(
        &format!("/Prev {start} >>\nstartxref\n{start}"),
        &format!("/Prev {second} >>\nstartxref\n{start}"),
    );
    let _ = open(patched.into_bytes());
}

#[test]
fn a_cross_reference_table_that_claims_billions_of_entries() {
    let mut bytes = Builder::document(&[b"0 g 0 0 10 10 re f"]);
    let start = bytes.len();
    bytes.extend_from_slice(
        format!("xref\n0 2000000000\n0000000000 65535 f \ntrailer\n<< /Size 2000000000 /Root 1 0 R >>\nstartxref\n{start}\n%%EOF\n")
            .as_bytes(),
    );
    let started = Instant::now();
    let _ = open(bytes);
    assert!(started.elapsed() < Duration::from_secs(20));
}

#[test]
fn nesting_a_million_levels_deep() {
    let depth = 1_000_000;
    let mut deep = "[".repeat(depth).into_bytes();
    deep.extend_from_slice("]".repeat(depth).as_bytes());
    // In a page's dictionary…
    let mut b = Builder::default();
    let catalog = b.reserve();
    let pages = b.reserve();
    let content = b.stream("", b"0 g 0 0 10 10 re f");
    let mut page = format!(
        "<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 200 100] /Contents {content} 0 R /Junk "
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
    let bytes = b.finish(catalog);
    if open(bytes.clone()).is_ok() {
        render_all(bytes, 1.0);
    }
    // …in a content stream, and dictionaries too.
    let mut content = b"0 g ".to_vec();
    content.extend_from_slice(&deep);
    content.extend_from_slice(" <<".repeat(depth).as_bytes());
    content.extend_from_slice(b" 0 0 10 10 re f");
    render_all(Builder::document(&[&content]), 1.0);
}

#[test]
fn a_form_that_draws_itself() {
    let mut b = Builder::default();
    let catalog = b.reserve();
    let pages = b.reserve();
    let form = b.reserve();
    let mut body = format!(
        "<< /Type /XObject /Subtype /Form /BBox [0 0 100 100] /Resources << /XObject << /F {form} 0 R >> >> /Length 30 >>\nstream\n"
    )
    .into_bytes();
    body.extend_from_slice(b"0 g 0 0 1 1 re f 0.9 0 0 0.9 1 1 cm /F Do\nendstream");
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
    let events = render_all(b.finish(catalog), 1.0);
    assert!(
        events.iter().any(|e| matches!(e, Event::Page(_))),
        "{events:?}"
    );
}

#[test]
fn a_million_saved_states_and_clips() {
    let mut content = Vec::new();
    for _ in 0..1_000_000 {
        content.extend_from_slice(b"q 0 0 200 100 re W n ");
    }
    content.extend_from_slice(b"0 g 0 0 10 10 re f");
    let packed = deflate(&content);
    let mut b = Builder::default();
    let catalog = b.reserve();
    let pages = b.reserve();
    let stream = b.stream("/Filter /FlateDecode", &packed);
    let page = b.add(format!(
        "<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 200 100] /Contents {stream} 0 R >>"
    ));
    b.set(catalog, format!("<< /Type /Catalog /Pages {pages} 0 R >>"));
    b.set(
        pages,
        format!("<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>"),
    );
    let events = render_all(b.finish(catalog), 1.0);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Page(_) | Event::PageFailed(_))),
        "{events:?}"
    );
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
        open_with(bytes.clone(), "", few_objects),
        Err(OpenError::TooLarge(Exceeded::Objects))
    );
    let few_pages = Limits {
        max_pages: 10,
        ..Limits::default()
    };
    assert_eq!(
        open_with(bytes.clone(), "", few_pages),
        Err(OpenError::TooLarge(Exceeded::Pages))
    );
    let small_file = Limits {
        max_file_bytes: 100,
        ..Limits::default()
    };
    assert_eq!(
        open_with(bytes, "", small_file),
        Err(OpenError::TooLarge(Exceeded::File))
    );
}

#[test]
fn large_render_requests_are_scaled_down() {
    let a4 = PageSize {
        width: 595.0,
        height: 842.0,
    };
    assert_eq!(fitted_scale(a4, 2.0, 40_000_000), 2.0);
    let fitted = fitted_scale(a4, 50.0, 40_000_000);
    let pixels = f64::from(595.0 * fitted) * f64::from(842.0 * fitted);
    assert!(pixels <= 40_000_000.0 * 1.001 && fitted > 7.0, "{fitted}");
    // A strip a mile long stays within 16 384 pixels.
    let strip = PageSize {
        width: 10.0,
        height: 14_400.0,
    };
    assert!(14_400.0 * fitted_scale(strip, 4.0, 40_000_000) <= 16_384.0);
}

#[test]
fn newer_requests_replace_older_ones() {
    let contents: Vec<&[u8]> = vec![b"0 g 0 0 10 10 re f"; 20];
    let (send, receive) = mpsc::channel();
    let worker = Worker::spawn(
        Builder::document(&contents),
        String::new(),
        Limits::default(),
        move |e| {
            let _ = send.send(e);
        },
    );
    assert!(matches!(
        receive.recv_timeout(Duration::from_secs(30)),
        Ok(Event::Opened(_))
    ));
    worker.want((0..20).map(|p| (p, 1.0)).collect());
    worker.want(vec![(7, 1.5)]);
    let mut seen = Vec::new();
    while let Ok(Event::Page(page)) = receive.recv_timeout(Duration::from_millis(500)) {
        seen.push((page.page, page.scale));
    }
    // Whatever was under way when the second request came, the last page sent
    // is the one asked for last, and nothing after it.
    assert_eq!(seen.last(), Some(&(7, 1.5)));
    assert!(seen.len() < 20);
}
