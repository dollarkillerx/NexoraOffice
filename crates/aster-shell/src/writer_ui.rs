//! The Word document tab in the window: keys, the pointer, scrolling, copy,
//! the find bar, the outline in the sidebar and the status bar.
//!
//! The view itself is `aster_surface::doc_view`, and the tab around it is
//! `aster_shell::writer_tab`; this module turns what the window reports into
//! calls on them and keeps the window's chrome in step. Documents are read-only
//! in this version: what would change the text does nothing.

use std::time::Duration;

use aster_shell::writer_tab::{DocTab, find_position};
use aster_surface::doc_view::Move;
use slint::{ComponentHandle, SharedString, TimerMode};

use super::{
    AppWindow, OutlineItem, Shell, deliver, host, i18n, model, present, refresh_status,
    refresh_toolbar,
};

/// How deep outline rows are indented at most, as levels.
const OUTLINE_INDENT: u8 = 8;

/// After the caret, the selection or the view moved: show it, and keep the
/// sidebar's outline marking the heading the caret is under.
pub(crate) fn after_move(ui: &AppWindow, shell: &mut Shell) {
    shell.wake_caret();
    sync_outline(ui, shell);
    present(ui, shell);
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
    // Control or Command with Home and End: the start and end of the document.
    let ends = single.and_then(|c| {
        if primary && c == char::from(Key::Home) {
            Some(Move::DocStart)
        } else if primary && c == char::from(Key::End) {
            Some(Move::DocEnd)
        } else {
            None
        }
    });
    let used = match (shell.active_doc(), ends) {
        (Some(tab), Some(movement)) => {
            tab.view.move_caret(movement, shift);
            true
        }
        (Some(tab), None) => match host::decode_key(text, shift, primary, alt) {
            Some(action) => tab.key(&action),
            None => false,
        },
        (None, _) => false,
    };
    if used {
        after_move(ui, shell);
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
    if let Some(tab) = shell.active_doc() {
        tab.pointer_down(x, y, clicks.clamp(1, 3), shift);
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
    let Some(url) = tab.pointer_up() else {
        return;
    };
    // A link to a place in the document (`#name`) has no place to go to
    // yet: bookmarks are kept in the file, not shown.
    if url.starts_with('#') {
        return;
    }
    // The same links as in the Markdown preview: web pages and e-mail only.
    let refused = match aster_platform::links::open(&url) {
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
    let Some(tab) = shell.active_doc() else {
        return;
    };
    let cursor = if tab.view.link_at(x, y).is_some() {
        5
    } else if tab.view.is_over_page(x, y) {
        4
    } else {
        0
    };
    if cursor != ui.get_resize_cursor() {
        ui.set_resize_cursor(cursor);
    }
}

/// The wheel or trackpad, in physical pixels.
pub(crate) fn scrolled(ui: &AppWindow, shell: &mut Shell, dx: f64, dy: f64) {
    if shell.active_doc().is_some_and(|tab| tab.view.wheel(dx, dy)) {
        present(ui, shell);
    }
}

/// Copy: the selected text. Control and format characters that change how
/// text reads without being seen stay off the clipboard; tabs and line
/// breaks are kept.
pub(crate) fn copy(ui: &AppWindow, shell: &mut Shell) {
    let Some(tab) = shell.active_doc() else {
        return;
    };
    let text: String = tab
        .view
        .selected_text()
        .chars()
        .filter(|c| matches!(c, '\n' | '\t') || !aster_pdf::clean::is_unwanted(*c))
        .collect();
    if !text.is_empty() {
        ui.invoke_copy_to_clipboard(text.into());
    }
}

/// Select all the text of the body.
pub(crate) fn select_all(ui: &AppWindow, shell: &mut Shell) {
    if shell.active_doc().is_some_and(|tab| tab.view.select_all()) {
        after_move(ui, shell);
    }
}

/// The status bar's zoom, format and notice for a Word document.
pub(crate) fn status(ui: &AppWindow, strings: &'static i18n::Strings, tab: &DocTab) {
    ui.set_status_zoom(format!("{}%", tab.view.zoom_percent() as u32).into());
    ui.set_zoom_enabled(true);
    let format = tab
        .path
        .extension()
        .and_then(|e| e.to_str())
        .map_or_else(|| "DOCX".to_owned(), str::to_ascii_uppercase);
    ui.set_status_format(format.into());
    ui.set_status_notice(if tab.link_refused {
        strings.notice_link_refused.into()
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

/// Mark another heading when the caret moved under one.
fn sync_outline(ui: &AppWindow, shell: &mut Shell) {
    let Some(tab) = shell.active_doc() else {
        return;
    };
    if tab.outline_position() != tab.outline_shown {
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
    ui.set_find_replace(false);
    ui.set_find_case(find.query.case_sensitive);
    ui.set_find_whole(find.query.whole_word);
    ui.set_find_regex(false);
    ui.set_find_error(false);
    ui.set_find_busy(false);
    ui.set_find_position(find_position(strings, tab).into());
    if ui.get_find_query().as_str() != find.query.pattern {
        ui.set_find_query(find.query.pattern.as_str().into());
    }
    true
}

/// Find bar actions on the Word document in front.
pub(crate) enum FindAction {
    Open,
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
