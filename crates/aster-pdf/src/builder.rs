//! Writes small PDF files for tests, hostile ones included. Shared by the
//! unit tests and the process tests (`tests/process.rs`).
#![allow(dead_code)]

use std::io::Write;

use flate2::Compression;
use flate2::write::ZlibEncoder;

/// Writes a PDF with a correct cross-reference table. Object `n` is the
/// `n`-th one added, counting from 1.
#[derive(Default)]
pub struct Builder {
    objects: Vec<Vec<u8>>,
}

impl Builder {
    pub fn add(&mut self, body: impl AsRef<[u8]>) -> usize {
        self.objects.push(body.as_ref().to_vec());
        self.objects.len()
    }

    pub fn stream(&mut self, dict: &str, data: &[u8]) -> usize {
        let mut body = format!("<< /Length {} {dict} >>\nstream\n", data.len()).into_bytes();
        body.extend_from_slice(data);
        body.extend_from_slice(b"\nendstream");
        self.add(body)
    }

    /// Reserve a number to fill in later (for loops).
    pub fn reserve(&mut self) -> usize {
        self.add(b"null")
    }

    pub fn set(&mut self, number: usize, body: impl AsRef<[u8]>) {
        self.objects[number - 1] = body.as_ref().to_vec();
    }

    pub fn finish(&self, root: usize) -> Vec<u8> {
        self.finish_with(root, "")
    }

    pub fn finish_with(&self, root: usize, trailer: &str) -> Vec<u8> {
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
    pub fn document(contents: &[&[u8]]) -> Vec<u8> {
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

pub fn deflate(data: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

/// `bytes` zeros, deflated, without holding them all.
pub fn zeros_deflated(bytes: usize) -> Vec<u8> {
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

/// A one-page document: `objects` adds what the page refers to and returns
/// its resource dictionary; the content stream has `content_dict` and
/// `content`.
pub fn one_page(
    objects: impl FnOnce(&mut Builder) -> String,
    content_dict: &str,
    content: &[u8],
) -> Vec<u8> {
    let mut b = Builder::default();
    let catalog = b.reserve();
    let pages = b.reserve();
    let resources = objects(&mut b);
    let stream = b.stream(content_dict, content);
    let page = b.add(format!(
        "<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 200 100] /Resources {resources} /Contents {stream} 0 R >>"
    ));
    b.set(catalog, format!("<< /Type /Catalog /Pages {pages} 0 R >>"));
    b.set(
        pages,
        format!("<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>"),
    );
    b.finish(catalog)
}

/// A one-page document drawing one picture, `/Im0`, with this dictionary
/// (after `/Type /XObject`) and data.
pub fn with_picture(dict: &str, data: &[u8]) -> Vec<u8> {
    one_page(
        |b| {
            let picture = b.stream(&format!("/Type /XObject {dict}"), data);
            format!("<< /XObject << /Im0 {picture} 0 R >> >>")
        },
        "",
        b"q 100 0 0 100 0 0 cm /Im0 Do Q",
    )
}

/// Forms nested `depth` deep, each drawing the next `fan` times: work that
/// doubles (at `fan` 2) with every level while every limit on nesting holds.
pub fn exponential(depth: usize, fan: usize) -> Vec<u8> {
    let mut b = Builder::default();
    let catalog = b.reserve();
    let pages = b.reserve();
    let mut inner = b.stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 100 100]",
        b"0 g 0 0 1 1 re f",
    );
    for _ in 0..depth {
        let draws = "/F Do ".repeat(fan);
        inner = b.stream(
            &format!("/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Resources << /XObject << /F {inner} 0 R >> >>"),
            draws.as_bytes(),
        );
    }
    let content = b.stream("", b"/F Do");
    let page = b.add(format!(
        "<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 200 100] /Resources << /XObject << /F {inner} 0 R >> >> /Contents {content} 0 R >>"
    ));
    b.set(catalog, format!("<< /Type /Catalog /Pages {pages} 0 R >>"));
    b.set(
        pages,
        format!("<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>"),
    );
    b.finish(catalog)
}

/// `pages` pages that all list the same content stream (`stream_dict` and
/// `data`) `listed` times in an array.
pub fn shared_content(pages: usize, listed: usize, stream_dict: &str, data: &[u8]) -> Vec<u8> {
    let mut b = Builder::default();
    let catalog = b.reserve();
    let tree = b.reserve();
    let shared = b.stream(stream_dict, data);
    let list = vec![format!("{shared} 0 R"); listed].join(" ");
    let mut kids = String::new();
    for _ in 0..pages {
        let page = b.add(format!(
            "<< /Type /Page /Parent {tree} 0 R /MediaBox [0 0 200 100] /Contents [{list}] >>"
        ));
        kids.push_str(&format!("{page} 0 R "));
    }
    b.set(
        tree,
        format!("<< /Type /Pages /Kids [{kids}] /Count {pages} >>"),
    );
    b.set(catalog, format!("<< /Type /Catalog /Pages {tree} 0 R >>"));
    b.finish(catalog)
}

/// One page, and an object nothing refers to: arrays `depth` deep around a
/// hex string of `digits` digits.
pub fn nested_hex(depth: usize, digits: usize) -> Vec<u8> {
    let mut b = Builder::default();
    let catalog = b.reserve();
    let tree = b.reserve();
    let content = b.stream("", b"0 g 0 0 10 10 re f");
    let page = b.add(format!(
        "<< /Type /Page /Parent {tree} 0 R /MediaBox [0 0 200 100] /Contents {content} 0 R >>"
    ));
    let mut deep = "[".repeat(depth).into_bytes();
    deep.push(b'<');
    deep.extend(std::iter::repeat_n(b'a', digits));
    deep.push(b'>');
    deep.extend_from_slice("]".repeat(depth).as_bytes());
    b.add(deep);
    b.set(
        tree,
        format!("<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>"),
    );
    b.set(catalog, format!("<< /Type /Catalog /Pages {tree} 0 R >>"));
    b.finish(catalog)
}

/// A document with a cross-reference stream whose object 6 lives in an
/// object stream (object 5, not compressed): `inner` is the body of object
/// 6. Object 4 is an ordinary content stream and the page's contents; the
/// page also refers to object 6 as `/Extra`.
pub fn with_object_stream(inner: &[u8]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    let mut put = |out: &mut Vec<u8>, number: usize, body: &[u8]| {
        offsets.push(out.len());
        out.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    };
    put(&mut out, 1, b"<< /Type /Catalog /Pages 2 0 R >>");
    put(&mut out, 2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    put(
        &mut out,
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Extra 6 0 R >>",
    );
    put(
        &mut out,
        4,
        b"<< /Length 18 >>\nstream\n0 g 0 0 10 10 re f\nendstream",
    );
    let head = b"6 0 ";
    let mut data = head.to_vec();
    data.extend_from_slice(inner);
    let body = [
        format!(
            "<< /Type /ObjStm /N 1 /First {} /Length {} >>\nstream\n",
            head.len(),
            data.len()
        )
        .into_bytes(),
        data,
        b"\nendstream".to_vec(),
    ]
    .concat();
    put(&mut out, 5, &body);
    let xref = out.len();
    let mut table = Vec::new();
    let mut entry = |kind: u8, a: u32, b: u16| {
        table.push(kind);
        table.extend_from_slice(&a.to_be_bytes());
        table.extend_from_slice(&b.to_be_bytes());
    };
    entry(0, 0, 65535);
    for offset in &offsets {
        entry(1, *offset as u32, 0);
    }
    entry(2, 5, 0);
    entry(1, xref as u32, 0);
    let mut stream = format!(
        "7 0 obj\n<< /Type /XRef /Size 8 /W [1 4 2] /Root 1 0 R /Length {} >>\nstream\n",
        table.len()
    )
    .into_bytes();
    stream.extend_from_slice(&table);
    stream.extend_from_slice(format!("\nendstream\nendobj\nstartxref\n{xref}\n%%EOF\n").as_bytes());
    out.extend_from_slice(&stream);
    out
}

// ---- Text, outline and search tests -------------------------------------

/// Resources for text tests: `/F1` Helvetica, `/F2` a non-embedded
/// Simplified Chinese font (UCS-2 text), `/F3` Identity-H with the GB1
/// collection and no `ToUnicode`.
pub const TEXT_RESOURCES: &str = "<< /Font << \
    /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> \
    /F2 << /Type /Font /Subtype /Type0 /BaseFont /STSong-Light /Encoding /UniGB-UCS2-H \
    /DescendantFonts [<< /Type /Font /Subtype /CIDFontType0 /BaseFont /STSong-Light \
    /CIDSystemInfo << /Registry (Adobe) /Ordering (GB1) /Supplement 4 >> /DW 1000 >>] >> \
    /F3 << /Type /Font /Subtype /Type0 /BaseFont /STSong-Light /Encoding /Identity-H \
    /DescendantFonts [<< /Type /Font /Subtype /CIDFontType0 /BaseFont /STSong-Light \
    /CIDSystemInfo << /Registry (Adobe) /Ordering (GB1) /Supplement 4 >> /DW 1000 >>] >> \
    >> >>";

/// A one-page document of `media_box` (like `0 0 200 100`) and `extra`
/// page entries (like `/Rotate 90`), with [`TEXT_RESOURCES`] and `content`.
pub fn text_page(media_box: &str, extra: &str, content: &[u8]) -> Vec<u8> {
    let mut b = Builder::default();
    let catalog = b.reserve();
    let pages = b.reserve();
    let stream = b.stream("", content);
    let page = b.add(format!(
        "<< /Type /Page /Parent {pages} 0 R /MediaBox [{media_box}] {extra} \
         /Resources {TEXT_RESOURCES} /Contents {stream} 0 R >>"
    ));
    b.set(catalog, format!("<< /Type /Catalog /Pages {pages} 0 R >>"));
    b.set(
        pages,
        format!("<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>"),
    );
    b.finish(catalog)
}
