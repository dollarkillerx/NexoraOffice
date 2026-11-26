//! The Word document tab in the window: keys, the pointer, scrolling, the
//! clipboard, saving, recovery records, the find bar with replace, the
//! outline in the sidebar and the status bar.
//!
//! The view itself is `aster_surface::doc_view`, and the tab around it is
//! `aster_shell::writer_tab`; this module turns what the window reports into
//! calls on them and keeps the window's chrome in step.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use aster_platform::dialogs::{self, FileType};
use aster_shell::recovery::{Candidate, RecordInfo};
use aster_shell::tabs::TabId;
use aster_shell::writer_tab::{
    DocPdf, DocSave, DocTab, PictureProblem, TableEdit, Written, find_position, read_picture,
};
use aster_storage::Contents;
use aster_surface::doc_view::{Align, Break, Corner, DocCommand, Handle, Move};
use slint::{ComponentHandle, SharedString, TimerMode};

use super::{
    AfterSave, AppWindow, Content, OutlineItem, Shell, deliver, files, host, i18n, model, present,
    refresh_chrome, refresh_sidebar, refresh_status, refresh_tabs, refresh_toolbar, show_dialog,
};

/// How deep outline rows are indented at most, as levels.
const OUTLINE_INDENT: u8 = 8;

/// After the caret, the selection or the view moved: show it, and keep the
/// sidebar's outline marking the heading the caret is under.
pub(crate) fn after_move(ui: &AppWindow, shell: &mut Shell) {
    shell.wake_caret();
    sync_table_flag(ui, shell);
    sync_outline(ui, shell);
    present(ui, shell);
}

/// Tell the window whether the caret is in a table, for the menu's table
/// items.
fn sync_table_flag(ui: &AppWindow, shell: &mut Shell) {
    let now = in_table(shell);
    if ui.get_in_table() != now {
        ui.set_in_table(now);
    }
}

/// How long typing has to pause before the words are counted again.
const COUNT_DELAY: Duration = Duration::from_millis(500);

/// After the document changed (or the caret moved by a key): the tab's
/// modified mark, the recovery record, the toolbar, the outline, the counts
/// and the picture.
pub(crate) fn after_edit(ui: &AppWindow, shell: &mut Shell) {
    shell.wake_caret();
    sync_table_flag(ui, shell);
    if shell.sync_modified() {
        refresh_tabs(ui, shell);
    }
    shell.sync_doc_records();
    // Rebuild the toolbar only when what it shows has changed.
    if let Some(tab) = shell.active_doc() {
        let now = (tab.look(), tab.view.can_undo(), tab.view.can_redo());
        if tab.shown_look.as_ref() != Some(&now) {
            tab.shown_look = Some(now);
            refresh_toolbar(ui, shell);
        }
    }
    sync_outline(ui, shell);
    schedule_count(ui, shell);
    refresh_find_bar(ui, shell);
    present(ui, shell);
}

/// Count the words again once typing has paused.
fn schedule_count(ui: &AppWindow, shell: &mut Shell) {
    let Some(tab) = shell.active_doc() else {
        return;
    };
    tab.stats_due = true;
    let weak = ui.as_weak();
    shell
        .doc_count_timer
        .start(TimerMode::SingleShot, COUNT_DELAY, move || {
            if let Some(ui) = weak.upgrade() {
                deliver(
                    ui,
                    Box::new(|ui, shell| {
                        if shell.active_doc().is_some_and(|tab| tab.recount()) {
                            refresh_status(ui, shell);
                        }
                    }),
                );
            }
        });
}

/// A key in a Word document. Returns whether it was used.
pub(crate) fn key(
    ui: &AppWindow,
    shell: &mut Shell,
    text: &str,
    shift: bool,
    primary: bool,
    alt: bool,
) -> bool {
    use slint::platform::Key;

    if text == "\u{1b}" {
        // Esc closes the find bar, then clears the selection.
        let Some(tab) = shell.active_doc() else {
            return false;
        };
        if tab.find.open {
            tab.close_find();
            super::after_find(ui, shell, None);
            return true;
        }
        if tab.view.has_selection() {
            tab.view.clear_selection();
            after_move(ui, shell);
            return true;
        }
        return false;
    }
    let mut chars = text.chars();
    let single = match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    };
    let is = |key: Key| single == Some(char::from(key));
    // Control or Command with Home and End: the start and end of the document.
    let ends = if primary && is(Key::Home) {
        Some(Move::DocStart)
    } else if primary && is(Key::End) {
        Some(Move::DocEnd)
    } else {
        None
    };
    // Command and Return: a page break. Command with L, E, R, J: alignment
    // (Word's), when the menu did not take them.
    let aligned = if primary && !shift {
        match text {
            "l" | "L" => Some(Align::Left),
            "e" | "E" => Some(Align::Center),
            "r" | "R" => Some(Align::Right),
            "j" | "J" => Some(Align::Justify),
            _ => None,
        }
    } else {
        None
    };
    let Some(tab) = shell.active_doc() else {
        return false;
    };
    let used = if let Some(movement) = ends {
        tab.view.move_caret(movement, shift);
        true
    } else if let Some(align) = aligned {
        tab.view.command(DocCommand::Align(align));
        true
    } else if primary && is(Key::Return) {
        tab.view.insert_break(Break::Page);
        true
    } else if is(Key::Backspace) || is(Key::Delete) {
        let action = if is(Key::Backspace) {
            aster_surface::Action::Backspace
        } else {
            aster_surface::Action::Delete
        };
        tab.key_with(&action, alt, primary)
    } else {
        match host::decode_key(text, shift, primary, alt) {
            Some(action) => tab.key(&action),
            None => false,
        }
    };
    if used {
        after_edit(ui, shell);
    }
    used
}

/// The pointer went down on the pages (logical to physical already).
pub(crate) fn pointer_down(
    ui: &AppWindow,
    shell: &mut Shell,
    x: f64,
    y: f64,
    shift: bool,
    clicks: u8,
) {
    shell.doc_drag_at = None;
    shell.drag_timer.stop();
    let primary = ui.get_pointer_primary();
    if let Some(tab) = shell.active_doc() {
        tab.pointer_down_with(x, y, clicks.clamp(1, 3), shift, primary);
    }
    refresh_status(ui, shell);
    after_move(ui, shell);
}

/// The pointer moved with the button down: the selection follows, and past
/// the top or bottom edge the window scrolls while the button is held.
pub(crate) fn pointer_drag(ui: &AppWindow, shell: &mut Shell, x: f64, y: f64) {
    let Some(tab) = shell.active_doc() else {
        return;
    };
    tab.pointer_drag(x, y);
    let height = f64::from(tab.view.size().1);
    if y < 0.0 || y > height {
        shell.doc_drag_at = Some((x, y));
        if !shell.drag_timer.running() {
            let weak = ui.as_weak();
            shell
                .drag_timer
                .start(TimerMode::Repeated, Duration::from_millis(40), move || {
                    if let Some(ui) = weak.upgrade() {
                        deliver(ui, Box::new(drag_scroll));
                    }
                });
        }
    } else {
        shell.doc_drag_at = None;
        shell.drag_timer.stop();
    }
    after_move(ui, shell);
}

/// One step of scrolling while a selection is dragged past the edge.
fn drag_scroll(ui: &AppWindow, shell: &mut Shell) {
    let Some((x, y)) = shell.doc_drag_at else {
        shell.drag_timer.stop();
        return;
    };
    let Some(tab) = shell.active_doc() else {
        shell.doc_drag_at = None;
        shell.drag_timer.stop();
        return;
    };
    if tab.view.drag_scroll(x, y) {
        after_move(ui, shell);
    }
}

/// The button went up: a click on a link opens it.
pub(crate) fn pointer_up(ui: &AppWindow, shell: &mut Shell) {
    shell.doc_drag_at = None;
    shell.drag_timer.stop();
    let Some(tab) = shell.active_doc() else {
        return;
    };
    let resizing = tab.view.is_resizing();
    let link = tab.pointer_up();
    if resizing {
        // The resize is one step of undo, made now.
        after_edit(ui, shell);
    }
    let Some(url) = link else {
        return;
    };
    // A link to a place in the document (`#name`) has no place to go to
    // yet: bookmarks are kept in the file, not shown.
    if url.starts_with('#') {
        return;
    }
    // The same links as in the Markdown preview: web pages and e-mail only.
    let refused = match open_address(&url) {
        Ok(()) => false,
        Err(aster_platform::links::OpenError::Refused) => true,
        Err(err) => {
            eprintln!("aster-shell: {err}");
            false
        }
    };
    if let Some(tab) = shell.active_doc() {
        tab.link_refused = refused;
    }
    refresh_status(ui, shell);
}

/// The pointer moved over the pages without a button down: an I-beam over a
/// page, a hand over a link (cursors 4 and 5 in the window's numbers).
pub(crate) fn hover(ui: &AppWindow, shell: &mut Shell, x: f64, y: f64) {
    let primary = ui.get_pointer_primary();
    let Some(tab) = shell.active_doc() else {
        return;
    };
    let link = if tab.view.is_resizing() {
        None
    } else {
        tab.view.link_at(x, y)
    };
    // The hand only with the key that opens links held; the corners of a
    // selected picture show the shape of what dragging them does.
    let cursor = match tab.view.handle_at(x, y) {
        Some(Handle::Corner(Corner::TopLeft | Corner::BottomRight)) => 6,
        Some(Handle::Corner(Corner::TopRight | Corner::BottomLeft)) => 7,
        Some(Handle::ColumnBorder) => 1,
        Some(Handle::RowBorder) => 2,
        // No cursor for moving yet: the pointer stays what the page makes it.
        Some(Handle::Move) => 0,
        None if link.is_some() && primary => 5,
        None if tab.view.is_over_page(x, y) => 4,
        None => 0,
    };
    let hint_changed = tab.hover_link != link;
    if hint_changed {
        tab.hover_link = link;
    }
    if cursor != ui.get_resize_cursor() {
        ui.set_resize_cursor(cursor);
    }
    if hint_changed {
        refresh_status(ui, shell);
    }
}

/// Open a link's address: web pages and e-mail only, as in the Markdown
/// preview; `#name` (a bookmark of the document) goes nowhere yet. Returns
/// whether the address was refused.
pub(crate) fn open_address(url: &str) -> Result<(), aster_platform::links::OpenError> {
    if url.starts_with('#') {
        return Ok(());
    }
    aster_platform::links::open(url)
}

/// The wheel or trackpad, in physical pixels.
pub(crate) fn scrolled(ui: &AppWindow, shell: &mut Shell, dx: f64, dy: f64) {
    if shell.active_doc().is_some_and(|tab| tab.view.wheel(dx, dy)) {
        present(ui, shell);
    }
}

/// What may go on the clipboard: control and format characters that change
/// how text reads without being seen stay off it; tabs and line breaks stay.
fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| matches!(c, '\n' | '\t') || !aster_pdf::clean::is_unwanted(*c))
        .collect()
}

/// Copy: the selected text goes on the clipboard, and the formatted
/// paragraphs are kept here for pasting into a Writer document.
pub(crate) fn copy(ui: &AppWindow, shell: &mut Shell) {
    let Some(tab) = shell.active_doc() else {
        return;
    };
    if tab.view.is_composing() {
        return;
    }
    let text = clean(&tab.view.selected_text());
    let fragment = tab.view.selected_fragment();
    if text.is_empty() {
        return;
    }
    shell.doc_clip = fragment.map(|blocks| (text.clone(), blocks));
    ui.invoke_copy_to_clipboard(text.into());
}

/// Cut: copy, then take the selection out.
pub(crate) fn cut(ui: &AppWindow, shell: &mut Shell) {
    let Some(tab) = shell.active_doc() else {
        return;
    };
    if tab.view.is_composing() {
        return;
    }
    let Some((text, fragment)) = tab.view.cut() else {
        return;
    };
    let text = clean(&text);
    shell.doc_clip = fragment.map(|blocks| (text.clone(), blocks));
    ui.invoke_copy_to_clipboard(text.into());
    after_edit(ui, shell);
}

/// Paste: the formatted paragraphs if the clipboard still holds the text
/// last copied from a Writer document, else the plain text.
pub(crate) fn paste(ui: &AppWindow, shell: &mut Shell) {
    // Everything waits for a composition to end.
    if shell
        .active_doc()
        .is_some_and(|tab| tab.view.is_composing())
    {
        return;
    }
    let text = ui.invoke_clipboard_text().to_string();
    if text.is_empty() {
        return;
    }
    let fragment = shell
        .doc_clip
        .as_ref()
        .filter(|(copied, _)| *copied == text)
        .map(|(_, blocks)| blocks.clone());
    let Some(tab) = shell.active_doc() else {
        return;
    };
    let changed = match fragment {
        Some(blocks) => tab.view.paste_fragment(&blocks),
        None => tab.view.paste_text(&text),
    };
    if changed {
        after_edit(ui, shell);
    }
}

/// Select all the text of the body.
pub(crate) fn select_all(ui: &AppWindow, shell: &mut Shell) {
    if shell.active_doc().is_some_and(|tab| tab.view.select_all()) {
        after_move(ui, shell);
    }
}

/// An entry chosen from a toolbar list.
pub(crate) fn option(
    ui: &AppWindow,
    shell: &mut Shell,
    command: aster_shell::toolbar::Command,
    index: usize,
) {
    if shell
        .active_doc()
        .is_some_and(|tab| tab.option(command, index))
    {
        after_edit(ui, shell);
    }
    ui.invoke_focus_document();
}

/// The Format menu's bold, italic, underline and strikethrough.
pub(crate) fn format_menu(ui: &AppWindow, shell: &mut Shell, code: i32) {
    use aster_shell::toolbar::Command;
    let command = match code {
        0 => Command::Bold,
        1 => Command::Italic,
        2 => Command::Underline,
        _ => Command::Strikethrough,
    };
    if shell.active_doc().is_some_and(|tab| tab.run(command)) {
        after_edit(ui, shell);
    }
}

/// The status bar's zoom, format and notice for a Word document.
pub(crate) fn status(ui: &AppWindow, strings: &'static i18n::Strings, tab: &DocTab) {
    ui.set_status_zoom(format!("{}%", tab.view.zoom_percent() as u32).into());
    ui.set_zoom_enabled(true);
    let format = tab
        .path
        .as_deref()
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .map_or_else(|| "DOCX".to_owned(), str::to_ascii_uppercase);
    ui.set_status_format(format.into());
    ui.set_status_notice(if tab.link_refused {
        strings.notice_link_refused.into()
    } else if let Some(address) = &tab.hover_link {
        let key = if cfg!(target_os = "macos") {
            "Cmd"
        } else {
            "Ctrl"
        };
        i18n::fill(
            &i18n::fill(strings.link_follow, "key", key),
            "address",
            address,
        )
        .into()
    } else {
        SharedString::new()
    });
}

/// The status bar's left side: the page, and the counts.
pub(crate) fn counts(ui: &AppWindow, strings: &'static i18n::Strings, tab: &DocTab) {
    use aster_shell::stats::group_digits;
    let (page, count) = tab.page_position();
    let position = i18n::fill(
        &i18n::fill(strings.pdf_page_position, "index", &group_digits(page)),
        "count",
        &group_digits(count),
    );
    let words = i18n::fill(strings.word_count, "count", &group_digits(tab.stats.words));
    let chars = i18n::fill(strings.char_count, "count", &group_digits(tab.stats.chars));
    ui.set_status_words(position.into());
    ui.set_status_chars(format!("{words} · {chars}").into());
    ui.set_status_ime(SharedString::new());
    ui.set_status_composing(false);
}

/// What the toolbar and status bar show of the page and zoom, if it changed
/// since they were last told.
pub(crate) fn follow_view(ui: &AppWindow, shell: &mut Shell) {
    let Some(tab) = shell.active_doc() else {
        return;
    };
    let shown = Some((tab.view.current_page(), tab.view.zoom_percent() as u32));
    if shown != shell.shown_doc {
        shell.shown_doc = shown;
        refresh_toolbar(ui, shell);
        refresh_status(ui, shell);
    }
}

// --- Outline ---

/// A heading as one line of the sidebar: objects and control characters
/// become spaces, runs of spaces one.
fn one_line(text: &str) -> String {
    let spaced: String = text
        .chars()
        .map(|c| {
            if c.is_control() || c == '\u{fffc}' {
                ' '
            } else {
                c
            }
        })
        .collect();
    spaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Show the outline of the Word document in front in the sidebar, the
/// heading the caret is under marked. False when the tab in front is not one.
pub(crate) fn refresh_outline(ui: &AppWindow, shell: &mut Shell) -> bool {
    let strings = shell.language.strings();
    let Some(tab) = shell.active_doc() else {
        return false;
    };
    let current = tab.outline_position();
    tab.outline_shown = current;
    tab.outline_rows = outline_rows(tab);
    let items: Vec<OutlineItem> = tab
        .view
        .outline()
        .iter()
        .enumerate()
        .map(|(index, entry)| OutlineItem {
            level: i32::from(entry.level.min(OUTLINE_INDENT)) + 1,
            text: one_line(&entry.text).into(),
            current: Some(index) == current,
            expandable: false,
            expanded: false,
        })
        .collect();
    let hint = if items.is_empty() {
        strings.doc_outline_none
    } else {
        ""
    };
    ui.set_outline_pdf(false);
    ui.set_has_outline(true);
    ui.set_outline_hint(hint.into());
    ui.set_outline(model(items));
    true
}

/// The headings as the sidebar shows them.
fn outline_rows(tab: &DocTab) -> Vec<(u8, String)> {
    tab.view
        .outline()
        .iter()
        .map(|entry| (entry.level, one_line(&entry.text)))
        .collect()
}

/// Mark another heading when the caret moved under one, and list the
/// headings again when they changed.
fn sync_outline(ui: &AppWindow, shell: &mut Shell) {
    let Some(tab) = shell.active_doc() else {
        return;
    };
    if tab.outline_position() != tab.outline_shown || outline_rows(tab) != tab.outline_rows {
        refresh_outline(ui, shell);
    }
}

/// A heading of the outline was clicked: the caret goes there.
pub(crate) fn outline_clicked(ui: &AppWindow, shell: &mut Shell, index: i32) {
    let Some(tab) = shell.active_doc() else {
        return;
    };
    let Some(pos) = usize::try_from(index)
        .ok()
        .and_then(|index| tab.view.outline().get(index))
        .map(|entry| entry.pos.clone())
    else {
        return;
    };
    tab.view.go_to(&pos);
    after_move(ui, shell);
}

// --- Find ---

/// The find bar of the tab in front, for `refresh_find`. False when the tab
/// in front is not a Word document.
pub(crate) fn refresh_find_bar(ui: &AppWindow, shell: &mut Shell) -> bool {
    let strings = shell.language.strings();
    let Some(tab) = shell.active_doc() else {
        return false;
    };
    let find = &tab.find;
    ui.set_find_open(find.open);
    ui.set_find_replace(find.replace_open);
    ui.set_find_case(find.query.case_sensitive);
    ui.set_find_whole(find.query.whole_word);
    ui.set_find_regex(false);
    ui.set_find_error(false);
    ui.set_find_busy(false);
    ui.set_find_position(find_position(strings, tab).into());
    if ui.get_find_query().as_str() != find.query.pattern {
        ui.set_find_query(find.query.pattern.as_str().into());
    }
    if ui.get_find_replacement().as_str() != find.replacement {
        ui.set_find_replacement(find.replacement.as_str().into());
    }
    true
}

/// Find bar actions on the Word document in front.
pub(crate) enum FindAction {
    Open,
    /// The bar with its replace row.
    OpenReplace,
    Replacement(String),
    ReplaceOne,
    ReplaceAll,
    /// The first line of the selection becomes the query.
    OpenWithSelection,
    Query(String),
    ToggleCase,
    ToggleWholeWord,
    Step(bool),
    Close,
}

/// Carry out a find bar action. False when the tab in front is not a Word
/// document (nothing was done).
pub(crate) fn find(ui: &AppWindow, shell: &mut Shell, action: FindAction) -> bool {
    let Some(tab) = shell.active_doc() else {
        return false;
    };
    match action {
        FindAction::Open => tab.open_find(),
        FindAction::OpenReplace => {
            tab.open_find();
            tab.find.replace_open = true;
        }
        FindAction::Replacement(text) => tab.find.replacement = text,
        FindAction::ReplaceOne => {
            if tab.view.is_composing() {
                return true;
            }
            let with = tab.find.replacement.clone();
            tab.view.replace_current(&with);
            after_edit(ui, shell);
            return true;
        }
        FindAction::ReplaceAll => {
            if tab.view.is_composing() {
                return true;
            }
            let with = tab.find.replacement.clone();
            tab.view.replace_all(&with);
            after_edit(ui, shell);
            return true;
        }
        FindAction::OpenWithSelection => {
            let text = tab.view.selected_text();
            tab.open_find();
            if let Some(line) = text.lines().map(str::trim).find(|line| !line.is_empty()) {
                tab.set_query(line.to_owned());
            }
        }
        FindAction::Query(text) => tab.set_query(text),
        FindAction::ToggleCase => tab.toggle_match_case(),
        FindAction::ToggleWholeWord => tab.toggle_whole_word(),
        FindAction::Step(forward) => {
            tab.find_step(forward);
        }
        FindAction::Close => tab.close_find(),
    }
    sync_outline(ui, shell);
    super::after_find(ui, shell, None);
    true
}

// --- Tables, links, pictures and the Format menu ---

/// The Format menu's codes (see `format` in `ui/app.slint`) for a Word
/// document: 8 link, 9 picture, 10 table, 30 to 35 and 39 the table's rows,
/// columns and the table itself.
pub(crate) fn format(ui: &AppWindow, shell: &mut Shell, code: i32) {
    let edit = match code {
        8 => return open_link_dialog(ui, shell),
        9 => return insert_picture(ui, shell),
        10 => return open_table_dialog(ui, shell),
        30 => TableEdit::RowAbove,
        31 => TableEdit::RowBelow,
        32 => TableEdit::ColumnLeft,
        33 => TableEdit::ColumnRight,
        34 => TableEdit::DeleteRow,
        35 => TableEdit::DeleteColumn,
        39 => TableEdit::DeleteTable,
        _ => return,
    };
    if shell.active_doc().is_some_and(|tab| tab.edit_table(edit)) {
        after_edit(ui, shell);
    }
}

/// Whether the caret of the Word document in front is in a table.
pub(crate) fn in_table(shell: &mut Shell) -> bool {
    shell
        .active_doc()
        .is_some_and(|tab| !tab.view.is_composing() && tab.view.in_table())
}

/// Open the link dialog on the selection or the link around the caret: its
/// text and address as they are, or empty for a new link.
pub(crate) fn open_link_dialog(ui: &AppWindow, shell: &mut Shell) {
    let Some(id) = shell.tabs.active_id() else {
        return;
    };
    let Some(Content::Doc(tab)) = shell.contents.get(&id) else {
        return;
    };
    if tab.view.is_composing() {
        return;
    }
    let info = tab.link_info();
    shell.doc_link_for = Some(id);
    ui.set_link_image(false);
    ui.set_link_label(info.text.into());
    ui.set_link_address(info.target.unwrap_or_default().into());
    ui.set_link_open(true);
}

/// OK in the link dialog of a Word document: the link is set (or taken off
/// when the address is empty), one step of undo. False when the dialog was
/// not for a Word document.
pub(crate) fn accept_link(ui: &AppWindow, shell: &mut Shell) -> bool {
    let Some(id) = shell.doc_link_for.take() else {
        return false;
    };
    ui.set_link_open(false);
    let text = ui.get_link_label().to_string();
    let address = ui.get_link_address().to_string();
    if let Some(Content::Doc(tab)) = shell.contents.get_mut(&id)
        && tab.edit_link(&text, &address)
    {
        after_edit(ui, shell);
    }
    ui.invoke_focus_document();
    true
}

/// Open the insert table dialog, three by three to start with.
pub(crate) fn open_table_dialog(ui: &AppWindow, shell: &mut Shell) {
    let Some(id) = shell.tabs.active_id() else {
        return;
    };
    let Some(Content::Doc(tab)) = shell.contents.get(&id) else {
        return;
    };
    if tab.view.is_composing() {
        return;
    }
    shell.doc_table_for = Some(id);
    ui.set_table_rows("3".into());
    ui.set_table_columns("3".into());
    ui.set_table_open(true);
}

/// OK in the insert table dialog. Numbers that are not whole numbers, or
/// are 0, keep the dialog open; larger ones than the limits are cut to them.
pub(crate) fn accept_table(ui: &AppWindow, shell: &mut Shell) {
    let number = |text: SharedString| text.trim().parse::<usize>().ok().filter(|n| *n > 0);
    let (Some(rows), Some(columns)) = (number(ui.get_table_rows()), number(ui.get_table_columns()))
    else {
        return;
    };
    ui.set_table_open(false);
    if let Some(id) = shell.doc_table_for.take()
        && let Some(Content::Doc(tab)) = shell.contents.get_mut(&id)
        && tab.insert_table(rows, columns)
    {
        after_edit(ui, shell);
    }
    ui.invoke_focus_document();
}

/// Ask for a picture file and put it in the document at the caret.
pub(crate) fn insert_picture(ui: &AppWindow, shell: &mut Shell) {
    let strings = shell.language.strings();
    let Some(id) = shell.tabs.active_id() else {
        return;
    };
    if !shell
        .active_doc()
        .is_some_and(|tab| !tab.view.is_composing())
    {
        return;
    }
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
                .map_err(|_| PictureProblem::NotAPicture)
                .and_then(|bytes| read_picture(&bytes).map(|size| (bytes, size)));
            let _ = weak.upgrade_in_event_loop(move |ui| {
                deliver(
                    ui,
                    Box::new(move |ui, shell| place_picture(ui, shell, id, &path, read)),
                );
            });
        });
    });
}

/// A picture file has been read: put it in the document, or say why not.
pub(crate) fn place_picture(
    ui: &AppWindow,
    shell: &mut Shell,
    id: TabId,
    path: &std::path::Path,
    read: Result<(Vec<u8>, (u32, u32)), PictureProblem>,
) {
    let strings = shell.language.strings();
    match read {
        Ok((bytes, size)) => {
            if let Some(Content::Doc(tab)) = shell.contents.get_mut(&id)
                && tab.insert_picture(bytes, size)
            {
                after_edit(ui, shell);
            }
        }
        Err(problem) => {
            let title = i18n::fill(strings.picture_failed, "name", &files::display_name(path));
            let body = match problem {
                PictureProblem::NotAPicture => strings.doc_picture_failed_body,
                PictureProblem::TooLarge => strings.picture_too_large_body,
            };
            show_dialog(ui, shell, 2, title, body.into());
        }
    }
}

// --- Export ---

/// File, Export as PDF for the Word document in front: ask where to, then
/// lay out and write on a worker thread, from a copy of the document taken
/// now. Errors are reported as the Markdown export reports them.
pub(crate) fn export_pdf(ui: &AppWindow, shell: &mut Shell) {
    let strings = shell.language.strings();
    let Some(id) = shell.tabs.active_id() else {
        return;
    };
    let Some((existing, suggested)) = shell.save_target(id) else {
        return;
    };
    let title = shell
        .tabs
        .get(id)
        .map(|tab| tab.title.clone())
        .unwrap_or_default();
    let name = std::path::Path::new(&title)
        .file_stem()
        .map_or(title.clone(), |stem| stem.to_string_lossy().into_owned());
    let Some(Content::Doc(tab)) = shell.contents.get(&id) else {
        return;
    };
    let job = tab.pdf_job(&name);
    let suggested = existing.unwrap_or(suggested).with_extension("pdf");
    let weak = ui.as_weak();
    let _ = slint::spawn_local(async move {
        let types = [FileType {
            name: strings.file_type_pdf,
            extensions: &["pdf"],
        }];
        let Some(path) =
            dialogs::pick_file_to_save(strings.menu_export_pdf, &suggested, &types).await
        else {
            return;
        };
        // Laying out and writing a long document takes a moment.
        std::thread::spawn(move || {
            if let Err(err) = write_pdf(&job, &path) {
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    deliver(
                        ui,
                        Box::new(move |ui, shell| export_failed(ui, shell, &path, err)),
                    );
                });
            }
        });
    });
}

/// Lay the document out and write it as a PDF file at `path`.
pub(crate) fn write_pdf(job: &DocPdf, path: &std::path::Path) -> Result<(), String> {
    let bytes = job.bytes()?;
    aster_storage::write_atomically(path, &bytes).map_err(|err| err.to_string())
}

/// Tell the user that the export to `path` failed.
pub(crate) fn export_failed(
    ui: &AppWindow,
    shell: &mut Shell,
    path: &std::path::Path,
    err: String,
) {
    let strings = shell.language.strings();
    let title = i18n::fill(strings.export_failed, "name", &files::display_name(path));
    show_dialog(ui, shell, 2, title, err);
}

// --- Saving ---

pub(crate) fn start_save(
    ui: &AppWindow,
    shell: &mut Shell,
    id: TabId,
    path: PathBuf,
    after: AfterSave,
) {
    let Some(Content::Doc(tab)) = shell.contents.get_mut(&id) else {
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
    job: DocSave,
    path: PathBuf,
    result: Result<Written, String>,
    after: AfterSave,
) {
    let ok = result.is_ok();
    let reason = result.as_ref().err().cloned();
    if let Some(Content::Doc(tab)) = shell.contents.get_mut(&id) {
        tab.finish_save(&job, &path, result.ok());
    }
    if ok {
        shell.tabs.rename(id, files::display_name(&path));
        shell.sync_modified();
        // Saved with nothing changed since: the record goes.
        shell.sync_doc_records();
        shell.remember_file(&path);
        refresh_sidebar(ui, shell);
        if after == AfterSave::CloseTab {
            shell.force_close(id);
            super::after_tab_change(ui, shell);
        } else {
            refresh_chrome(ui, shell);
            present(ui, shell);
        }
    } else {
        // Nothing was changed in the tab: the document is as it was, still
        // modified, and can be saved again.
        let strings = shell.language.strings();
        let title = i18n::fill(strings.save_failed, "name", &files::display_name(&path));
        show_dialog(ui, shell, 2, title, reason.unwrap_or_default());
        refresh_chrome(ui, shell);
    }
}

// --- Recovery records ---

impl Shell {
    /// What the recovery record of a document says about it.
    fn doc_record_info(&self, id: TabId, tab: &DocTab) -> RecordInfo {
        RecordInfo {
            title: self
                .tabs
                .get(id)
                .map(|tab| tab.title.clone())
                .unwrap_or_default(),
            path: tab.path.clone(),
            format: aster_markdown::TextFormat::default(),
            disk: tab.disk,
            workbook: false,
            presentation: false,
            document: true,
        }
    }

    /// Write each Word document with unsaved changes to its recovery record,
    /// as saving would write it (made on the writer thread), at most every
    /// few seconds; a record goes once its document has no unsaved changes.
    pub(crate) fn sync_doc_records(&mut self) {
        let ids: Vec<TabId> = self.contents.keys().copied().collect();
        for id in ids {
            let Some(Content::Doc(tab)) = self.contents.get(&id) else {
                continue;
            };
            let info = self.doc_record_info(id, tab);
            let Some(writer) = &self.writer else {
                return;
            };
            let Some(Content::Doc(tab)) = self.contents.get_mut(&id) else {
                continue;
            };
            if !tab.is_modified() {
                if let Some(key) = tab.record.take() {
                    writer.discard(key);
                }
                tab.recorded = None;
                continue;
            }
            let revision = tab.view.revision();
            let every = tab.record_every();
            match tab.recorded {
                Some((done, _)) if done == revision && tab.record.is_some() => continue,
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
            tab.recorded = Some((revision, std::time::Instant::now()));
        }
    }

    /// Bring back a Word document from its record: the file as it was last
    /// written, in a tab of its own with unsaved changes, saving to the file
    /// it belongs to. The record is kept writing.
    pub(crate) fn restore_document(&mut self, candidate: Candidate) -> Option<TabId> {
        let Candidate {
            record,
            info: Some(info),
            workbook: Some(bytes),
            ..
        } = candidate
        else {
            return None;
        };
        let mut document = aster_ooxml::docx::read(bytes.clone()).ok()?;
        document.renumber();
        self.leave_active_document();
        let title = if info.title.is_empty() {
            info.path
                .as_deref()
                .map_or_else(String::new, files::display_name)
        } else {
            info.title.clone()
        };
        let id = self.tabs.open(aster_shell::tabs::DocKind::Writer, title);
        let source = Arc::new(bytes);
        let mut tab = DocTab::new(
            aster_surface::doc_view::DocView::new(document),
            source.clone(),
            info.path.clone(),
            &self.tokens,
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
            tab.recorded = Some((tab.view.revision(), std::time::Instant::now()));
        }
        self.contents.insert(id, Content::Doc(Box::new(tab)));
        self.sync_modified();
        Some(id)
    }
}
