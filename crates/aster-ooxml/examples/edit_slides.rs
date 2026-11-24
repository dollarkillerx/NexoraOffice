//! Open a presentation, make edits as the Slides tab would, save a copy, read
//! it back and list which parts changed: to check what saving changes.
//!
//! `cargo run -p aster-ooxml --example edit_slides -- in.pptx out.pptx [edit ...]`
//!
//! Slides and shapes count from 1; shapes are the slide's own, in drawing
//! order. Edits:
//!
//! - `type S N TEXT`: add TEXT at the end of shape N's text (a new line
//!   with `\n`);
//! - `bold S N`: make all of shape N's text bold;
//! - `move S N DX DY`: move shape N by DX, DY EMU;
//! - `rect S`: add a rectangle in the middle of slide S;
//! - `add L`: add a slide on layout L at the end;
//! - `copy S`: copy slide S to the end;
//! - `delete S`: delete slide S;
//! - `notes S TEXT`: give slide S these notes.
//!
//! With no edits the file is saved as read, which must change no part.
//! `blank` as the input starts from a new presentation.

use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::sync::Arc;

use aster_ooxml::pptx_save::save;
use aster_slides::edit::TextPos;
use aster_slides::{Fill, Frame, Geometry, Outline, Role, Shape};

fn parts(bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).expect("a zip file");
    (0..archive.len())
        .map(|i| {
            let mut entry = archive.by_index(i).expect("an entry");
            let mut data = Vec::new();
            entry.read_to_end(&mut data).expect("its bytes");
            (entry.name().to_owned(), data)
        })
        .collect()
}

fn number(text: Option<&String>) -> usize {
    text.and_then(|t| t.parse::<usize>().ok())
        .filter(|n| *n >= 1)
        .unwrap_or_else(|| usage())
        - 1
}

fn usage() -> ! {
    eprintln!(
        "usage: edit_slides <in.pptx> <out.pptx> [type S N TEXT | bold S N | move S N DX DY | rect S | add L | copy S | delete S | notes S TEXT] ..."
    );
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(input), Some(output)) = (args.first(), args.get(1)) else {
        usage();
    };
    let source = if input == "blank" {
        aster_ooxml::pptx_save::blank::presentation(&[
            "Title Slide",
            "Title and Content",
            "Section Header",
            "Title Only",
            "Blank",
        ])
    } else {
        std::fs::read(input).expect("read")
    };
    let mut presentation = aster_ooxml::pptx::read(source.clone()).expect("decode");
    let mut rest = args[2..].iter();
    while let Some(command) = rest.next() {
        let own = |presentation: &mut aster_slides::Presentation, s: usize, n: usize| -> usize {
            presentation.slides[s]
                .shapes
                .iter()
                .enumerate()
                .filter(|(_, shape)| !shape.inherited)
                .nth(n)
                .map(|(i, _)| i)
                .expect("no such shape")
        };
        match command.as_str() {
            "type" => {
                let (s, n) = (number(rest.next()), number(rest.next()));
                let text = rest.next().unwrap_or_else(|| usage()).replace("\\n", "\n");
                let i = own(&mut presentation, s, n);
                let shape = &mut presentation.slides[s].shapes[i];
                let body = shape.text.get_or_insert_with(Default::default);
                let end = body.end();
                body.insert(end, &text);
            }
            "bold" => {
                let (s, n) = (number(rest.next()), number(rest.next()));
                let i = own(&mut presentation, s, n);
                if let Some(body) = presentation.slides[s].shapes[i].text.as_mut() {
                    let end = body.end();
                    body.format(TextPos::default(), end, |run| run.bold = true);
                }
            }
            "move" => {
                let (s, n) = (number(rest.next()), number(rest.next()));
                let dx: i64 = rest
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| usage());
                let dy: i64 = rest
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| usage());
                let i = own(&mut presentation, s, n);
                let frame = &mut presentation.slides[s].shapes[i].frame;
                frame.x += dx;
                frame.y += dy;
            }
            "rect" => {
                let s = number(rest.next());
                let (w, h) = presentation.size;
                let slide = &mut presentation.slides[s];
                let mut shape = Shape::new(
                    Frame::new(w / 2 - 1_000_000, h / 2 - 500_000, 2_000_000, 1_000_000),
                    Geometry::Rect,
                    Role::Shape,
                );
                shape.fill = Some(Fill::Solid(slide.look.accent));
                shape.line = Some(Outline {
                    color: slide.look.accent_line,
                    width: 12_700,
                });
                slide.shapes.push(shape);
            }
            "add" => {
                let l = number(rest.next());
                let slide = presentation
                    .layouts
                    .get(l)
                    .expect("no such layout")
                    .slide
                    .clone();
                presentation.slides.push(slide);
            }
            "copy" => {
                let s = number(rest.next());
                let mut copy = presentation.slides[s].clone();
                if let Some(source) = &copy.source {
                    let mut source = (**source).clone();
                    source.copy = true;
                    copy.source = Some(Arc::new(source));
                }
                presentation.slides.push(copy);
            }
            "delete" => {
                let s = number(rest.next());
                presentation.slides.remove(s);
            }
            "notes" => {
                let s = number(rest.next());
                let text = rest.next().unwrap_or_else(|| usage()).replace("\\n", "\n");
                presentation.slides[s].notes = Some(text);
            }
            _ => usage(),
        }
    }
    let bytes = save(source.clone(), &presentation).expect("save");
    std::fs::write(output, &bytes).expect("write");
    let again = aster_ooxml::pptx::read(bytes.clone()).expect("the saved file reads");
    let (before, after) = (parts(&source), parts(&bytes));
    let mut same = 0;
    for name in before
        .keys()
        .chain(after.keys().filter(|k| !before.contains_key(*k)))
    {
        match (before.get(name), after.get(name)) {
            (Some(a), Some(b)) if a == b => same += 1,
            (Some(_), Some(_)) => println!("changed  {name}"),
            (Some(_), None) => println!("removed  {name}"),
            (None, Some(_)) => println!("added    {name}"),
            (None, None) => {}
        }
    }
    println!("{same} of {} parts byte for byte the same", before.len());
    for (i, slide) in again.slides.iter().enumerate() {
        let notes = slide.notes.as_deref().map_or(String::new(), |n| {
            format!(" — notes: {}", n.replace('\n', " / "))
        });
        println!(
            "slide {}: {}{notes}",
            i + 1,
            slide.title().unwrap_or_default()
        );
    }
}
