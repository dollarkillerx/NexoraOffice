//! Open a document, make a few visible edits as the Writer tab would, and
//! save a copy that can be looked at in other programs.
//!
//! `cargo run -p aster-ooxml --example edit_docx -- in.docx out.docx`
//!
//! The edits: a sentence typed at the end of the first paragraph, the first
//! words of the second made bold, the third paragraph split in two, and a
//! bullet list of three items added at the end; then, in the first table of
//! the document if it has one, a row and a column added and the last row
//! removed (its text is gone with it); a link on the first two words of the
//! first paragraph that has them; a small picture at the end of the first
//! paragraph; and a table of two rows and three columns after the first
//! paragraph. `blank` as the input starts from a new document.
//!
//! With `--stories` as a third argument nothing but the end of the first
//! header and of the first footnote is typed into, and the saved file is
//! checked: it reads back with that text in the same story, the text of the
//! body is as it was, and no part but the header's and the footnotes' part
//! differs from the file that was opened (byte for byte). The last line says
//! what was done: `stories header=<part or -> footnote=<id or ->`.
//!
//! With `--new` a header is made (or the first section's default one is
//! used, if it has one) and typed into, and a footnote is made at the end
//! of the body and typed into; the file is checked as above, with the parts
//! a new header, footer or footnote needs allowed to differ besides.

use aster_ooxml::docx_save::{blank, save};
use aster_writer::edit::{
    ListKind, apply_run_patch, delete_rows, ensure_marginal, insert_columns, insert_footnote,
    insert_picture, insert_rows, insert_table, insert_text, set_link, set_list, split_paragraph,
};
use aster_writer::{
    Block, Document, Link, MarginalKind, MarginalSlot, Path, Pos, RunPatch, StoryId, paragraphs_of,
};

/// The text typed by `--stories`.
const HEADER_MARK: &str = " [header edited]";
const NOTE_MARK: &str = " [note edited]";

/// The index of the first header (not footer) any section shows.
fn first_header(document: &Document) -> Option<usize> {
    document.sections().iter().find_map(|(_, section)| {
        [
            section.headers.default,
            section.headers.first,
            section.headers.even,
        ]
        .into_iter()
        .flatten()
        .find(|index| *index < document.marginals.len())
    })
}

const NEW_HEADER_MARK: &str = "Header made with Aster Office";
const NEW_NOTE_MARK: &str = "Note made with Aster Office";

/// The parts a save may change besides the edited story parts when headers
/// and footnotes are made: the main part, its relationships, the content
/// types, and parts of headers, footers and footnotes.
fn may_differ(name: &str, main: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    name == main
        || name == "[Content_Types].xml"
        || name == aster_ooxml::package::rels_name(main)
        || (lower.starts_with("word/")
            && ["header", "footer", "footnotes"]
                .iter()
                .any(|stem| lower.contains(stem)))
}

/// Make a header and a footnote, type into them, save, and check the file.
fn creations(source: Vec<u8>, output: &str) {
    let mut document = aster_ooxml::docx::read(source.clone()).expect("a Word document");
    let original = document.clone();
    let fail = |what: &str| -> ! {
        eprintln!("new stories check failed: {what}");
        std::process::exit(1);
    };
    let story = ensure_marginal(
        &mut document,
        0,
        MarginalKind::Header,
        MarginalSlot::Default,
    )
    .unwrap_or_else(|| fail("no first section"));
    let made = original.marginals.len() < document.marginals.len();
    let end = document
        .story_end(story)
        .unwrap_or_else(|| fail("an empty header"));
    insert_text(&mut document, &end, NEW_HEADER_MARK, None);
    // A body with no paragraph has nowhere for the reference: only the
    // header is made.
    let mut id = None;
    if let Some(end) = document.end() {
        let selection = insert_footnote(&mut document, &end)
            .unwrap_or_else(|| fail("no footnote could be made"));
        insert_text(&mut document, &selection.focus, NEW_NOTE_MARK, None);
        let StoryId::Footnote(made) = selection.focus.path.story else {
            fail("the caret is not in the note");
        };
        id = Some(made);
    }
    let saved = save(source.clone(), &document).expect("the document saves");
    let back = aster_ooxml::docx::read(saved.clone()).expect("the saved file reads");
    let Some(index) = back.sections()[0].1.headers.default else {
        fail("the section has no header");
    };
    let text = |back: &Document, story| {
        back.paragraphs_in(story)
            .last()
            .map(|(_, p)| p.text())
            .unwrap_or_default()
    };
    if !text(&back, StoryId::Marginal(index)).ends_with(NEW_HEADER_MARK) {
        fail("the header does not read back with the text");
    }
    let reference = if id.is_some() {
        aster_writer::OBJECT
    } else {
        ""
    };
    if let Some(id) = id
        && !text(&back, StoryId::Footnote(id)).ends_with(NEW_NOTE_MARK)
    {
        fail("the footnote does not read back with the text");
    }
    if back.plain_text() != format!("{}{reference}", original.plain_text()) {
        fail("the body is not the old one with the reference at its end");
    }
    let mut before = aster_ooxml::Package::open(source.clone()).expect("a package");
    let mut after = aster_ooxml::Package::open(saved.clone()).expect("a package");
    let main = before
        .expect(aster_ooxml::Kind::Docx)
        .unwrap_or_else(|_| fail("not a Word document"));
    let mut names = before.part_names();
    names.extend(after.part_names());
    names.sort();
    names.dedup();
    for name in names {
        if before.read(&name).ok() != after.read(&name).ok() && !may_differ(&name, &main) {
            fail(&format!("{name} changed"));
        }
    }
    std::fs::write(output, saved).expect("the output file is written");
    println!(
        "new header={} footnote={}",
        if made { "made" } else { "existing" },
        id.map_or("-".to_owned(), |id| id.to_string())
    );
}

/// Type into the first header and the first footnote, save, and check the
/// file. Exits with an error message when something does not hold.
fn stories(source: Vec<u8>, output: &str) {
    let mut document = aster_ooxml::docx::read(source.clone()).expect("a Word document");
    let original = document.clone();
    let mut header_part = None;
    if let Some(index) = first_header(&document)
        && let Some(end) = document.story_end(StoryId::Marginal(index))
    {
        insert_text(&mut document, &end, HEADER_MARK, None);
        header_part = original.marginals[index].part.as_deref().map(str::to_owned);
    }
    let mut note = None;
    if let Some(first) = document.footnotes.notes.first() {
        let id = first.id;
        if let Some(end) = document.story_end(StoryId::Footnote(id)) {
            insert_text(&mut document, &end, NOTE_MARK, None);
            note = Some(id);
        }
    }
    let saved = save(source.clone(), &document).expect("the document saves");

    let back = aster_ooxml::docx::read(saved.clone()).expect("the saved file reads");
    let fail = |what: &str| -> ! {
        eprintln!("stories check failed: {what}");
        std::process::exit(1);
    };
    if let Some(part) = &header_part {
        let found = back
            .marginals
            .iter()
            .position(|m| m.part.as_deref() == Some(part.as_str()))
            .unwrap_or_else(|| fail("the header is gone"));
        let text = back
            .paragraphs_in(StoryId::Marginal(found))
            .last()
            .map(|(_, p)| p.text())
            .unwrap_or_default();
        if !text.ends_with(HEADER_MARK) {
            fail("the header does not read back with the edit");
        }
    }
    if let Some(id) = note {
        let text = back
            .paragraphs_in(StoryId::Footnote(id))
            .last()
            .map(|(_, p)| p.text())
            .unwrap_or_default();
        if !text.ends_with(NOTE_MARK) {
            fail("the footnote does not read back with the edit");
        }
    }
    if back.plain_text() != original.plain_text() {
        fail("the body changed");
    }
    // Only the parts that were edited differ.
    let mut expected: Vec<String> = Vec::new();
    expected.extend(header_part.clone());
    if note.is_some() {
        expected.extend(original.footnotes.part.as_deref().map(str::to_owned));
    }
    let mut before = aster_ooxml::Package::open(source.clone()).expect("a package");
    let mut after = aster_ooxml::Package::open(saved.clone()).expect("a package");
    let mut names = before.part_names();
    names.extend(after.part_names());
    names.sort();
    names.dedup();
    let mut differing = Vec::new();
    for name in names {
        let (a, b) = (before.read(&name).ok(), after.read(&name).ok());
        if a != b {
            differing.push(name);
        }
    }
    expected.sort();
    if differing != expected {
        fail(&format!(
            "parts that differ: {differing:?}, expected {expected:?}"
        ));
    }
    std::fs::write(output, saved).expect("the output file is written");
    println!(
        "stories header={} footnote={}",
        header_part.as_deref().unwrap_or("-"),
        note.map_or("-".to_owned(), |id| id.to_string())
    );
}

/// A small RGB PNG (a gradient), written with stored deflate blocks.
fn png(width: u32, height: u32) -> Vec<u8> {
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend((data.len() as u32).to_be_bytes());
        let mut checked = kind.to_vec();
        checked.extend(data);
        out.extend(&checked);
        out.extend(crc32(&checked).to_be_bytes());
    }
    let mut raw = Vec::new();
    for y in 0..height {
        raw.push(0);
        for x in 0..width {
            raw.extend([
                (x * 255 / width.max(1)) as u8,
                (y * 255 / height.max(1)) as u8,
                160,
            ]);
        }
    }
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65_535).collect();
    for (i, block) in blocks.iter().enumerate() {
        z.push(u8::from(i + 1 == blocks.len()));
        z.extend((block.len() as u16).to_le_bytes());
        z.extend((!(block.len() as u16)).to_le_bytes());
        z.extend(*block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for byte in &raw {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    z.extend(((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend(width.to_be_bytes());
    header.extend(height.to_be_bytes());
    header.extend([8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// The start of the first paragraph of the table that is body block `table`.
fn in_table(
    document: &aster_writer::Document,
    table: usize,
    row: usize,
    cell: usize,
) -> Option<Pos> {
    let t = document.body.blocks.get(table)?.as_table()?;
    let blocks = &t.rows.get(row)?.cells.get(cell)?.blocks;
    let (inner, _) = paragraphs_of(blocks).into_iter().next()?;
    let mut steps = Path::body(table).steps;
    steps.extend([row as u32, cell as u32]);
    steps.extend(inner.steps);
    Some(Pos::new(Path::from(steps), 0))
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(input), Some(output)) = (args.next(), args.next()) else {
        eprintln!("usage: edit_docx in.docx out.docx [--stories | --new]");
        std::process::exit(2);
    };
    let mode = args.next();
    let source = if input == "blank" {
        blank()
    } else {
        std::fs::read(&input).expect("the input file reads")
    };
    match mode.as_deref() {
        Some("--stories") => return stories(source, &output),
        Some("--new") => return creations(source, &output),
        _ => {}
    }
    let mut document = aster_ooxml::docx::read(source.clone()).expect("a Word document");

    let paths: Vec<_> = document
        .paragraphs()
        .into_iter()
        .map(|(path, p)| (path, p.len()))
        .collect();
    let (first, first_len) = paths.first().cloned().expect("a paragraph");
    insert_text(
        &mut document,
        &Pos::new(first, first_len),
        " Edited with Aster Office: 在这里输入了一句话。",
        None,
    );
    if let Some((second, len)) = paths.get(1).cloned() {
        let to = len.min(8);
        // On a char boundary: the operations move the end to one.
        apply_run_patch(
            &mut document,
            &Pos::new(second.clone(), 0),
            &Pos::new(second, to),
            &RunPatch {
                bold: Some(true),
                ..RunPatch::default()
            },
        );
    }
    if let Some((third, len)) = paths.get(2).cloned() {
        split_paragraph(&mut document, &Pos::new(third, len / 2));
    }
    let end = document.end().expect("an end");
    let at = split_paragraph(&mut document, &end);
    let at = insert_text(
        &mut document,
        &at,
        "First point\nSecond point\nThird point",
        None,
    );
    let start = Pos::new(
        {
            let mut path = at.path.clone();
            if let Some(last) = path.steps.last_mut() {
                *last = last.saturating_sub(2);
            }
            path
        },
        0,
    );
    set_list(&mut document, &start, &at, Some(ListKind::Bullet));

    // The first table: a row and a column added, another row removed.
    let first_table = document
        .body
        .blocks
        .iter()
        .position(|b| matches!(b, Block::Table(_)));
    if let Some(table) = first_table
        && let Some(at) = in_table(&document, table, 0, 0)
    {
        insert_rows(&mut document, &at, &at, false);
        insert_columns(&mut document, &at, &at, false);
        let rows = document
            .body
            .blocks
            .get(table)
            .and_then(Block::as_table)
            .map_or(0, |t| t.rows.len());
        if rows > 2
            && let Some(last) = in_table(&document, table, rows - 1, 0)
        {
            delete_rows(&mut document, &last, &last);
        }
    }

    // A link on the first two words of the first paragraph with two words.
    let words = document.paragraphs().into_iter().find_map(|(path, p)| {
        let text = p.text();
        let mut spans = text.split_whitespace().map(|w| {
            let start = w.as_ptr() as usize - text.as_ptr() as usize;
            (start, start + w.len())
        });
        let (first, second) = (spans.next()?, spans.next()?);
        Some((path, first.0, second.1))
    });
    if let Some((path, from, to)) = words {
        set_link(
            &mut document,
            &Pos::new(path.clone(), from),
            &Pos::new(path, to),
            Some(Link {
                target: "https://example.org/".to_owned(),
                tooltip: None,
            }),
        );
    }

    // A small picture at the end of the first paragraph, and a table after it.
    let first = document
        .paragraphs()
        .first()
        .map(|(path, p)| (path.clone(), p.len()))
        .expect("a paragraph");
    let end = Pos::new(first.0, first.1);
    let end = insert_picture(
        &mut document,
        &end,
        png(48, 32).into(),
        (914_400, 609_600),
        Some("A gradient".to_owned()),
    )
    .expect("a PNG is a picture");
    insert_table(&mut document, &end, 2, 3, 6_000);

    let saved = save(source, &document).expect("the document saves");
    std::fs::write(&output, saved).expect("the output file is written");
    println!("wrote {output}");
}
