//! The Slides tab in the window: keys, the pointer, the toolbar and the
//! Slide and Format menus, the notes field, saving, and the slide show.
//!
//! The editing itself is in `aster_surface::slide_view`; this module turns
//! what the window reports into calls on it and keeps the window's chrome
//! (toolbar, menus, tab marks, status bar) in step.

use std::path::PathBuf;
use std::sync::Arc;

use aster_markdown::TextFormat as FileFormat;
use aster_shell::recovery::{Candidate, RecordInfo};
use aster_storage::Contents;

use aster_platform::dialogs::{self, FileType};
use aster_shell::office::{self, SlidesTab};
use aster_shell::tabs::TabId;
use aster_shell::toolbar::{self, CellLook, Command};
use aster_slides::Align;
use aster_surface::slide_view::{CaretMove, Cursor, NewShape, Selection, TextFormat};
use slint::{ComponentHandle, SharedString};

use super::{
    AfterSave, AppWindow, Content, Shell, deliver, files, i18n, is_typed, present, refresh_chrome,
    refresh_sidebar, refresh_status, refresh_tabs, refresh_toolbar, refresh_viewer, show_dialog,
};

/// After anything changed on the slides: tab marks, toolbar, notes,
/// status bar and the picture.
pub(crate) fn after_change(ui: &AppWindow, shell: &mut Shell) {
    if shell.sync_modified() {
        refresh_tabs(ui, shell);
    }
    shell.sync_slide_records();
    refresh_toolbar(ui, shell);
    refresh_viewer(ui, shell);
    refresh_status(ui, shell);
    shell.wake_caret();
    present(ui, shell);
}

/// The window's Slides state: notes, what the menus act on.
pub(crate) fn refresh(ui: &AppWindow, tab: &SlidesTab) {
    let view = &tab.view;
    let notes = view
        .presentation()
        .slides
        .get(view.current())
        .and_then(|slide| slide.notes.clone())
        .unwrap_or_default();
    // The notes field holds what is typed: only set when the slide's
    // notes are something else, so the caret there stays put.
    if ui.get_slide_notes().as_str().trim_end() != notes.trim_end() {
        ui.set_slide_notes(notes.into());
    }
    ui.set_slide_text(view.look().is_some());
    ui.set_slide_shape(view.selection().shape().is_some());
}

/// How the text the toolbar acts on looks, for its buttons: as a cell
/// look, whether there is any, its list, whether there is a slide.
pub(crate) fn toolbar_state(tab: &SlidesTab) -> (CellLook, bool, u8, bool) {
    let has_slide = tab.view.count() > 0;
    let Some(look) = tab.view.look() else {
        return (CellLook::default(), false, 0, has_slide);
    };
    let cell = CellLook {
        bold: look.bold,
        italic: look.italic,
        underline: look.underline,
        align: match look.align {
            Align::Left | Align::Justify => 1,
            Align::Center => 2,
            Align::Right => 3,
        },
        size: look.size,
        font_color: look.color,
        ..CellLook::default()
    };
    let list = if look.numbered {
        2
    } else if look.bullets {
        1
    } else {
        0
    };
    (cell, true, list, has_slide)
}

/// The lists of the New Slide and Shapes buttons.
pub(crate) fn fill_lists(items: &mut [toolbar::Item], tab: &SlidesTab, strings: &i18n::Strings) {
    for item in items {
        match item.command {
            Some(Command::NewSlide) => {
                item.options = tab
                    .view
                    .presentation()
                    .layouts
                    .iter()
                    .map(|layout| layout.name.clone())
                    .collect();
            }
            Some(Command::InsertShape) => {
                item.options = [
                    strings.shape_rect,
                    strings.shape_round_rect,
                    strings.shape_ellipse,
                    strings.shape_line,
                ]
                .iter()
                .map(|s| (*s).to_owned())
                .collect();
            }
            _ => {}
        }
    }
}

/// A toolbar or Edit menu command on the presentation in front. Returns
/// whether anything changed.
pub(crate) fn run(shell: &mut Shell, command: Command) -> bool {
    let Some(tab) = shell.active_slides() else {
        return false;
    };
    let view = &mut tab.view;
    let format = |format| Some(format);
    let format = match command {
        Command::Undo => return view.undo(),
        Command::Redo => return view.redo(),
        Command::NewSlide => {
            let layout = next_layout(tab);
            return tab.view.add_slide(layout);
        }
        Command::InsertTextBox => return view.insert_shape(NewShape::TextBox),
        Command::Bold => format(TextFormat::Bold),
        Command::Italic => format(TextFormat::Italic),
        Command::Underline => format(TextFormat::Underline),
        Command::Strikethrough => format(TextFormat::Strike),
        Command::FontSizeUp => format(TextFormat::SizeStep(true)),
        Command::FontSizeDown => format(TextFormat::SizeStep(false)),
        Command::FontSize => format(TextFormat::Size(18.0)),
        Command::AlignLeft => format(TextFormat::Align(Align::Left)),
        Command::AlignCenter => format(TextFormat::Align(Align::Center)),
        Command::AlignRight => format(TextFormat::Align(Align::Right)),
        Command::BulletList => format(TextFormat::Bullets),
        Command::NumberedList => format(TextFormat::Numbering),
        _ => None,
    };
    format.is_some_and(|format| tab.view.format(format))
}

/// The layout a new slide takes: the current slide's, except after a
/// title slide, where it is the next layout (title and content), as in
/// PowerPoint.
fn next_layout(tab: &SlidesTab) -> usize {
    let presentation = tab.view.presentation();
    let current = presentation
        .slides
        .get(tab.view.current())
        .and_then(|slide| slide.layout);
    match current {
        Some(0) | None if presentation.layouts.len() > 1 => 1,
        Some(layout) => layout,
        None => 0,
    }
}

/// A choice from a toolbar list: a layout, a shape, a text colour.
pub(crate) fn option(ui: &AppWindow, shell: &mut Shell, command: Command, index: usize) {
    let Some(tab) = shell.active_slides() else {
        return;
    };
    let changed = match command {
        Command::NewSlide => tab.view.add_slide(index),
        Command::InsertShape => {
            let kind = match index {
                0 => NewShape::Rect,
                1 => NewShape::RoundRect,
                2 => NewShape::Ellipse,
                _ => NewShape::Line,
            };
            tab.view.insert_shape(kind)
        }
        Command::TextColor => match toolbar::palette_color(index) {
            Some(color) => tab.view.format(TextFormat::Color(color)),
            None => false,
        },
        _ => false,
    };
    if changed {
        after_change(ui, shell);
    }
}

/// The Slide menu (see `slide-command` in `app.slint`).
pub(crate) fn menu(ui: &AppWindow, shell: &mut Shell, item: i32) {
    if item == 9 {
        start_show(ui, shell);
        return;
    }
    let Some(tab) = shell.active_slides() else {
        return;
    };
    let view = &mut tab.view;
    let changed = match item {
        0 => {
            let layout = next_layout(tab);
            tab.view.add_slide(layout)
        }
        1 => view.duplicate_slide(),
        2 => view.delete_slide(),
        // With the thumbnails in focus, Cmd+D copies the slide.
        3 if view.strip_has_focus() => view.duplicate_slide(),
        3 => view.duplicate_shape(),
        4 => view.delete_shape(),
        5 => view.order_shape(true),
        6 => view.order_shape(false),
        7 => view.format(TextFormat::Indent(true)),
        8 => view.format(TextFormat::Indent(false)),
        _ => false,
    };
    if changed {
        after_change(ui, shell);
    }
}

/// The Format menu's bold, italic, underline, strikethrough (0 to 3).
pub(crate) fn format_menu(ui: &AppWindow, shell: &mut Shell, item: i32) {
    let command = match item {
        0 => Command::Bold,
        1 => Command::Italic,
        2 => Command::Underline,
        _ => Command::Strikethrough,
    };
    if run(shell, command) {
        after_change(ui, shell);
    }
}

/// The notes of the current slide were typed into.
pub(crate) fn notes_edited(ui: &AppWindow, shell: &mut Shell, text: &str) {
    let Some(tab) = shell.active_slides() else {
        return;
    };
    if tab.view.set_notes(text) {
        if shell.sync_modified() {
            refresh_tabs(ui, shell);
        }
        shell.sync_slide_records();
        refresh_toolbar(ui, shell);
    }
}

/// A key on the slide. Returns whether it was taken.
pub(crate) fn key(
    ui: &AppWindow,
    shell: &mut Shell,
    text: &str,
    shift: bool,
    primary: bool,
    alt: bool,
) -> bool {
    use slint::platform::Key;
    let Some(tab) = shell.active_slides() else {
        return false;
    };
    let is = |key: Key| text.chars().eq(std::iter::once(char::from(key)));
    let view = &mut tab.view;
    let editing = view.is_editing_text();
    let selected = matches!(view.selection(), Selection::Shape(_));
    let caret = |left: CaretMove, word: CaretMove, line: CaretMove| {
        if primary {
            line
        } else if alt {
            word
        } else {
            left
        }
    };
    // Cmd+B, Cmd+I, Cmd+U, when the Format menu did not take them.
    if primary && (editing || selected) {
        let command = match text {
            "b" | "B" => Some(Command::Bold),
            "i" | "I" => Some(Command::Italic),
            "u" | "U" => Some(Command::Underline),
            _ => None,
        };
        if let Some(command) = command {
            if run(shell, command) {
                after_change(ui, shell);
            }
            return true;
        }
    }
    let Some(tab) = shell.active_slides() else {
        return false;
    };
    let view = &mut tab.view;
    let changed = if is(Key::Escape) {
        view.escape()
    } else if is(Key::Backspace) || is(Key::Delete) {
        view.delete(is(Key::Delete))
    } else if editing && is(Key::Return) {
        view.enter(shift)
    } else if editing && (is(Key::Tab) || is(Key::Backtab)) {
        view.tab(shift || is(Key::Backtab))
    } else if editing && is(Key::LeftArrow) {
        view.move_caret(
            caret(CaretMove::Left, CaretMove::WordLeft, CaretMove::LineStart),
            shift,
        )
    } else if editing && is(Key::RightArrow) {
        view.move_caret(
            caret(CaretMove::Right, CaretMove::WordRight, CaretMove::LineEnd),
            shift,
        )
    } else if editing && is(Key::UpArrow) {
        view.move_caret(
            if primary {
                CaretMove::Start
            } else {
                CaretMove::Up
            },
            shift,
        )
    } else if editing && is(Key::DownArrow) {
        view.move_caret(
            if primary {
                CaretMove::End
            } else {
                CaretMove::Down
            },
            shift,
        )
    } else if editing && is(Key::Home) {
        view.move_caret(
            if primary {
                CaretMove::Start
            } else {
                CaretMove::LineStart
            },
            shift,
        )
    } else if editing && is(Key::End) {
        view.move_caret(
            if primary {
                CaretMove::End
            } else {
                CaretMove::LineEnd
            },
            shift,
        )
    } else if editing && primary && (text == "a" || text == "A") {
        view.select_all()
    } else if (editing || selected) && is_typed(text) && !primary {
        view.type_text(text)
    } else if selected && is(Key::Return) {
        view.enter(false)
    } else if selected && let Some((dx, dy)) = nudge(text, if alt { 12_700 } else { 76_200 }) {
        view.nudge(dx, dy)
    } else if let Some(key) = super::viewer_key(text) {
        office::slide_key(view, key, shift)
    } else {
        return false;
    };
    if changed {
        after_change(ui, shell);
    }
    true
}

/// Arrow keys move a selected shape by `step` EMU.
fn nudge(text: &str, step: i64) -> Option<(i64, i64)> {
    use slint::platform::Key;
    let is = |key: Key| text.chars().eq(std::iter::once(char::from(key)));
    if is(Key::LeftArrow) {
        Some((-step, 0))
    } else if is(Key::RightArrow) {
        Some((step, 0))
    } else if is(Key::UpArrow) {
        Some((0, -step))
    } else if is(Key::DownArrow) {
        Some((0, step))
    } else {
        None
    }
}

/// A press of the button on the slide or its thumbnails, physical pixels.
pub(crate) fn pointer_down(
    ui: &AppWindow,
    shell: &mut Shell,
    x: f64,
    y: f64,
    shift: bool,
    clicks: u8,
) {
    let Some(tab) = shell.active_slides() else {
        return;
    };
    if tab.view.press(x, y, shift, u32::from(clicks)) {
        after_change(ui, shell);
    }
}

pub(crate) fn pointer_drag(ui: &AppWindow, shell: &mut Shell, x: f64, y: f64) {
    let shift = ui.get_drag_shift();
    let Some(tab) = shell.active_slides() else {
        return;
    };
    if tab.view.drag(x, y, shift) {
        present(ui, shell);
    }
}

pub(crate) fn pointer_up(ui: &AppWindow, shell: &mut Shell) {
    let Some(tab) = shell.active_slides() else {
        return;
    };
    if tab.view.release() {
        after_change(ui, shell);
    }
}

pub(crate) fn hover(ui: &AppWindow, shell: &mut Shell, x: f64, y: f64) {
    let Some(tab) = shell.active_slides() else {
        return;
    };
    let cursor = match tab.view.cursor_at(x, y) {
        Cursor::Default => 0,
        Cursor::Text => 1,
        Cursor::Move => 2,
        Cursor::ResizeX => 3,
        Cursor::ResizeY => 4,
        Cursor::ResizeDown => 5,
        Cursor::ResizeUp => 6,
    };
    if cursor != ui.get_slide_cursor() {
        ui.set_slide_cursor(cursor);
    }
}

// --- Pictures ---

/// Ask for a picture and put it on the current slide.
pub(crate) fn insert_picture(ui: &AppWindow, shell: &mut Shell) {
    let strings = shell.language.strings();
    let Some(id) = shell.tabs.active_id() else {
        return;
    };
    let weak = ui.as_weak();
    let _ = slint::spawn_local(async move {
        let types = [FileType {
            name: strings.file_type_picture,
            extensions: &["png", "jpg", "jpeg"],
        }];
        let Some(path) = dialogs::pick_file_to_open(strings.insert_picture_dialog, &types).await
        else {
            return;
        };
        // Read and measured on a worker thread: a photo can be large.
        std::thread::spawn(move || {
            let read = std::fs::read(&path)
                .ok()
                .and_then(|bytes| {
                    let kind = aster_image::sniff(&bytes);
                    let usable = matches!(kind, aster_image::Kind::Png | aster_image::Kind::Jpeg);
                    usable.then_some(bytes)
                })
                .and_then(|bytes| {
                    let image = aster_image::decode(&bytes, 64).ok()?;
                    let (w, h) = picture_size(&bytes).unwrap_or((image.width, image.height));
                    Some((bytes, (w, h)))
                });
            let _ = weak.upgrade_in_event_loop(move |ui| {
                deliver(
                    ui,
                    Box::new(move |ui, shell| match read {
                        Some((bytes, size)) => {
                            if let Some(Content::Slides(tab)) = shell.contents.get_mut(&id)
                                && tab.view.insert_picture(Arc::from(bytes), size)
                            {
                                after_change(ui, shell);
                            }
                        }
                        None => {
                            let title = i18n::fill(
                                strings.picture_failed,
                                "name",
                                &files::display_name(&path),
                            );
                            show_dialog(ui, shell, 2, title, strings.picture_failed_body.into());
                        }
                    }),
                );
            });
        });
    });
}

/// A PNG's or JPEG's size in pixels, from its header.
fn picture_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.starts_with(b"\x89PNG") && bytes.len() >= 24 {
        let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
        let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
        return Some((w, h));
    }
    if bytes.starts_with(&[0xFF, 0xD8]) {
        let mut at = 2;
        while at + 9 < bytes.len() {
            if bytes[at] != 0xFF {
                at += 1;
                continue;
            }
            let marker = bytes[at + 1];
            let length = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
            // Start of frame markers carry the size.
            if matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
                let h = u16::from_be_bytes([bytes[at + 5], bytes[at + 6]]);
                let w = u16::from_be_bytes([bytes[at + 7], bytes[at + 8]]);
                return Some((u32::from(w), u32::from(h)));
            }
            at += 2 + length;
        }
    }
    None
}

// --- Saving ---

/// Write a presentation to `path` on a worker thread.
pub(crate) fn start_save(
    ui: &AppWindow,
    shell: &mut Shell,
    id: TabId,
    path: PathBuf,
    after: AfterSave,
) {
    let Some(Content::Slides(tab)) = shell.contents.get_mut(&id) else {
        return;
    };
    let Some(job) = tab.begin_save() else {
        return;
    };
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let result = job.write(&path);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            deliver(
                ui,
                Box::new(move |ui, shell| finish_save(ui, shell, id, job, path, result, after)),
            );
        });
    });
}

fn finish_save(
    ui: &AppWindow,
    shell: &mut Shell,
    id: TabId,
    job: office::SlideSave,
    path: PathBuf,
    result: Result<Vec<u8>, String>,
    after: AfterSave,
) {
    if let Some(Content::Slides(tab)) = shell.contents.get_mut(&id) {
        tab.finish_save(&job, &path, result.as_ref().ok().map(Vec::as_slice));
    }
    match result {
        Ok(_) => {
            shell.tabs.rename(id, files::display_name(&path));
            shell.sync_modified();
            // Saved with nothing changed since: the record goes.
            shell.sync_slide_records();
            shell.remember_file(&path);
            refresh_sidebar(ui, shell);
            if after == AfterSave::CloseTab {
                shell.force_close(id);
                super::after_tab_change(ui, shell);
            } else {
                refresh_chrome(ui, shell);
                present(ui, shell);
            }
        }
        Err(reason) => {
            let strings = shell.language.strings();
            let title = i18n::fill(strings.save_failed, "name", &files::display_name(&path));
            show_dialog(ui, shell, 2, title, reason);
        }
    }
}

// --- The slide show ---

pub(crate) fn start_show(ui: &AppWindow, shell: &mut Shell) {
    let Some(tab) = shell.active_slides() else {
        return;
    };
    if tab.view.count() == 0 {
        return;
    }
    tab.view.start_show();
    ui.set_presenting(true);
    ui.window().set_fullscreen(true);
    present_show(ui, shell);
}

fn end_show(ui: &AppWindow, shell: &mut Shell) {
    if let Some(tab) = shell.active_slides() {
        tab.view.end_show();
    }
    ui.set_presenting(false);
    ui.window().set_fullscreen(false);
    after_change(ui, shell);
    ui.invoke_focus_document();
}

/// A key in the slide show.
pub(crate) fn show_key(ui: &AppWindow, shell: &mut Shell, text: &str, shift: bool) {
    use slint::platform::Key;
    let is = |key: Key| text.chars().eq(std::iter::once(char::from(key)));
    let Some(tab) = shell.active_slides() else {
        end_show(ui, shell);
        return;
    };
    if is(Key::Escape) {
        end_show(ui, shell);
        return;
    }
    let back = is(Key::LeftArrow)
        || is(Key::UpArrow)
        || is(Key::PageUp)
        || is(Key::Backspace)
        || (shift && (text == " " || is(Key::Return)));
    let forward = is(Key::RightArrow)
        || is(Key::DownArrow)
        || is(Key::PageDown)
        || text == " "
        || is(Key::Return)
        || text == "n"
        || text == "N";
    if back {
        tab.view.show_step(false);
    } else if forward && tab.view.show_step(true).is_none() {
        end_show(ui, shell);
        return;
    } else if is(Key::Home) {
        tab.view.go_to(0);
    } else if is(Key::End) {
        tab.view.last_slide();
    } else {
        return;
    }
    present_show(ui, shell);
}

pub(crate) fn show_click(ui: &AppWindow, shell: &mut Shell) {
    let Some(tab) = shell.active_slides() else {
        return;
    };
    if tab.view.show_step(true).is_none() {
        end_show(ui, shell);
        return;
    }
    present_show(ui, shell);
}

/// Draw the slide show if one is running. Returns whether it is.
pub(crate) fn present_show(ui: &AppWindow, shell: &mut Shell) -> bool {
    let scale = ui.window().scale_factor();
    let width = aster_surface::host::to_px(ui.get_show_width(), scale);
    let height = aster_surface::host::to_px(ui.get_show_height(), scale);
    let Some(tab) = shell.active_slides() else {
        return false;
    };
    if !tab.view.is_presenting() {
        return false;
    }
    tab.view.resize(width.max(1), height.max(1), scale);
    if let Some(frame) = aster_surface::host::render_frame(&mut tab.view) {
        ui.set_show_frame(frame);
    }
    true
}

/// The window's own Edit menu on a slide: copy, cut, paste, select all.
pub(crate) fn edit(ui: &AppWindow, shell: &mut Shell, what: Edit) -> bool {
    let Some(tab) = shell.active_slides() else {
        return false;
    };
    let changed = match what {
        Edit::Copy | Edit::Cut => {
            if let Some(text) = tab.view.copy(what == Edit::Cut) {
                ui.invoke_copy_to_clipboard(text.into());
            }
            what == Edit::Cut
        }
        Edit::Paste => {
            let text: SharedString = ui.invoke_clipboard_text();
            match shell.active_slides() {
                Some(tab) => tab.view.paste(text.as_str()),
                None => false,
            }
        }
        Edit::SelectAll => tab.view.select_all(),
    };
    if changed {
        after_change(ui, shell);
    } else {
        present(ui, shell);
    }
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Edit {
    Copy,
    Cut,
    Paste,
    SelectAll,
}

// --- Recovery records ---

impl Shell {
    /// What the recovery record of a presentation says about it.
    fn slide_record_info(&self, id: TabId, tab: &SlidesTab) -> RecordInfo {
        RecordInfo {
            title: self
                .tabs
                .get(id)
                .map(|tab| tab.title.clone())
                .unwrap_or_default(),
            path: tab.path.clone(),
            format: FileFormat::default(),
            disk: tab.disk,
            workbook: false,
            presentation: true,
            document: false,
        }
    }

    /// Write each presentation with unsaved changes to its recovery record,
    /// as saving would write it (made on the writer thread), at most every
    /// few seconds; a record goes once its presentation has no unsaved
    /// changes. Called after edits and by a timer, as for workbooks.
    pub(crate) fn sync_slide_records(&mut self) {
        let ids: Vec<TabId> = self.contents.keys().copied().collect();
        for id in ids {
            let Some(Content::Slides(tab)) = self.contents.get(&id) else {
                continue;
            };
            let info = self.slide_record_info(id, tab);
            let Some(writer) = &self.writer else {
                return;
            };
            let Some(Content::Slides(tab)) = self.contents.get_mut(&id) else {
                continue;
            };
            if !tab.is_modified() {
                if let Some(key) = tab.record.take() {
                    writer.discard(key);
                }
                tab.recorded = None;
                continue;
            }
            let version = tab.view.version();
            let every = tab.record_every();
            match tab.recorded {
                Some((done, _)) if done == version && tab.record.is_some() => continue,
                Some((_, at)) if at.elapsed() < every => continue,
                _ => {}
            }
            let contents = Contents::maybe(tab.snapshot());
            match tab.record {
                Some(key) => {
                    let meta = info.to_meta();
                    let remove = RecordInfo::optional_keys()
                        .into_iter()
                        .filter(|optional| !meta.iter().any(|(key, _)| key == optional))
                        .collect();
                    writer.set_meta(key, meta, remove);
                    writer.checkpoint(key, contents);
                }
                None => {
                    let key = self.next_key;
                    self.next_key += 1;
                    writer.open(key, info.to_meta(), contents);
                    tab.record = Some(key);
                }
            }
            tab.recorded = Some((version, std::time::Instant::now()));
        }
    }

    /// Bring back a presentation from its record: the file as it was last
    /// written, in a tab of its own with unsaved changes, saving to the file
    /// it belongs to. The record is kept writing.
    pub(crate) fn restore_presentation(&mut self, candidate: Candidate) -> Option<TabId> {
        let Candidate {
            record,
            info: Some(info),
            workbook: Some(bytes),
            ..
        } = candidate
        else {
            return None;
        };
        let presentation = aster_ooxml::pptx::read(bytes.clone()).ok()?;
        self.leave_active_document();
        let title = if info.title.is_empty() {
            info.path
                .as_deref()
                .map_or_else(String::new, files::display_name)
        } else {
            info.title.clone()
        };
        let id = self.tabs.open(aster_shell::tabs::DocKind::Slides, title);
        let source = Arc::new(bytes);
        let strings = self.language.strings();
        let mut tab = SlidesTab::new(
            presentation,
            source.clone(),
            info.path.clone(),
            &self.tokens,
            strings,
        );
        tab.disk = info.disk;
        tab.mark_recovered();
        if let Some(writer) = &self.writer {
            let key = self.next_key;
            self.next_key += 1;
            writer.adopt(
                key,
                record,
                info.to_meta(),
                Contents::Bytes((*source).clone()),
            );
            tab.record = Some(key);
            tab.recorded = Some((tab.view.version(), std::time::Instant::now()));
        }
        self.contents.insert(id, Content::Slides(Box::new(tab)));
        self.sync_modified();
        Some(id)
    }
}
