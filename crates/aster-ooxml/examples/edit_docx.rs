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
//! With `shape` as a third argument the document also gets: in the first
//! table, the first two cells of the first row merged, a column border
//! moved, the first row made taller and the last cell of the last row split
//! in two; and the first drawing it has made to float with the text
//! wrapping around it, moved a little, and made a little larger.

use aster_ooxml::docx_save::{blank, save};
use aster_writer::edit::{
    ListKind, PictureLayout, apply_run_patch, delete_rows, insert_columns, insert_picture,
    insert_rows, insert_table, insert_text, merge_cells, move_column_border, move_floating_drawing,
    resize_drawing, set_link, set_list, set_picture_layout, set_row_height, split_cell,
    split_paragraph,
};
use aster_writer::{Block, Inline, Link, Path, Pos, RowHeight, RunPatch, paragraphs_of};

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
    let mut steps = Path::body(table).0;
    steps.extend([row as u32, cell as u32]);
    steps.extend(inner.0);
    Some(Pos::new(Path(steps), 0))
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(input), Some(output)) = (args.next(), args.next()) else {
        eprintln!("usage: edit_docx in.docx out.docx");
        std::process::exit(2);
    };
    let source = if input == "blank" {
        blank()
    } else {
        std::fs::read(&input).expect("the input file reads")
    };
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
            if let Some(last) = path.0.last_mut() {
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

    if std::env::args().nth(3).as_deref() == Some("shape") {
        shape(&mut document, first_table);
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

/// The extra edits of `shape`: see the top of the file.
fn shape(document: &mut aster_writer::Document, first_table: Option<usize>) {
    if let Some(table) = first_table {
        let cells = |document: &aster_writer::Document, row: usize| {
            document
                .body
                .blocks
                .get(table)
                .and_then(Block::as_table)
                .and_then(|t| t.rows.get(row))
                .map_or(0, |r| r.cells.len())
        };
        if let (Some(a), Some(b)) = (
            in_table(document, table, 0, 0),
            in_table(document, table, 0, 1),
        ) {
            merge_cells(document, &a, &b);
            set_row_height(document, &a, &a, RowHeight::AtLeast(500));
        }
        move_column_border(document, &Path::body(table), 1, 200);
        let rows = document
            .body
            .blocks
            .get(table)
            .and_then(Block::as_table)
            .map_or(0, |t| t.rows.len());
        if rows > 0 {
            let last = cells(document, rows - 1);
            if last > 0
                && let Some(at) = in_table(document, table, rows - 1, last - 1)
            {
                split_cell(document, &at, 2, 1);
            }
        }
    }
    // The first drawing: where it is, as a position before its object.
    let found = document.paragraphs().into_iter().find_map(|(path, p)| {
        let mut start = 0;
        for run in &p.runs {
            if matches!(run.kind, Inline::Drawing(_)) {
                return Some(Pos::new(path, start));
            }
            start += run.text.len();
        }
        None
    });
    if let Some(at) = found {
        set_picture_layout(document, &at, PictureLayout::Square);
        move_floating_drawing(document, &at, (91_440, 45_720), (0, 0));
        if let Some(d) = aster_writer::edit::drawing_at(document, &at) {
            let size = (d.size.0 + d.size.0 / 10, d.size.1 + d.size.1 / 10);
            resize_drawing(document, &at, size);
        }
    }
}
