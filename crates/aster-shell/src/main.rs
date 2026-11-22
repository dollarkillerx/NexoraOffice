//! The Aster Office shell: binds the shell logic in the library half of this
//! crate to the Slint UI under `ui/`, and hosts the custom-drawn text surface
//! from `aster-surface` in Writer tabs.
//!
//! Input-method events never reach a Slint `FocusScope`, so they are taken from
//! winit through Slint's `unstable-winit-030` window-event hook, exactly as the
//! Phase 0 gate G1 spike does.

use std::cell::RefCell;
use std::collections::HashMap;
use std::process::ExitCode;
use std::rc::Rc;
use std::time::{Duration, Instant};

use aster_shell::cli::{self, Options};
use aster_shell::i18n::{self, Language};
use aster_shell::icons::Icon;
use aster_shell::sample::{RECENT_FILES, WORKSPACE_TREE};
use aster_shell::stats::{ImeState, TextStats, group_digits};
use aster_shell::tabs::{DocKind, TabId, Tabs};
use aster_shell::theme::{self, Rgba, ThemeMode};
use aster_shell::toolbar::{self, Command, Control, ViewState, zoom_step};
use aster_surface::host::{self, to_px};
use aster_surface::{ImeEvent, PageLayout, Palette, TextSurface};
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use slint::{Color, ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};

slint::include_modules!();

#[cfg(target_os = "macos")]
mod macos;

const CARET_BLINK: Duration = Duration::from_millis(530);
/// Width kept free for the window buttons when the tab strip is the title bar.
const WINDOW_BUTTONS_INSET: f32 = 76.0;

/// A Writer tab's content. Other kinds have no content yet.
struct Document {
    surface: TextSurface,
    zoom_percent: u32,
    stats: TextStats,
}

impl Document {
    fn new(text: &str, tokens: &theme::Tokens) -> Self {
        let mut surface = TextSurface::new(text);
        surface.set_palette(palette(tokens));
        surface.set_page_layout(page_layout(tokens));
        Self {
            stats: TextStats::of(text),
            surface,
            zoom_percent: 100,
        }
    }
}

struct Shell {
    tabs: Tabs,
    documents: HashMap<TabId, Document>,
    tokens: theme::Tokens,
    language: Language,
    ime: ImeState,
    caret_on: bool,
    last_blink: Instant,
}

impl Shell {
    fn new(mode: ThemeMode, language: Language) -> Self {
        Self {
            tabs: Tabs::new(),
            documents: HashMap::new(),
            tokens: theme::Tokens::of(mode),
            language,
            ime: ImeState::default(),
            caret_on: true,
            last_blink: Instant::now(),
        }
    }

    /// The Writer document in front, if that is what is in front.
    fn active_document(&mut self) -> Option<&mut Document> {
        let id = self.tabs.active_id()?;
        self.documents.get_mut(&id)
    }

    fn wake_caret(&mut self) {
        self.caret_on = true;
        self.last_blink = Instant::now();
    }

    /// The tab in front is about to change: a composition must not be left
    /// behind in a document that can no longer receive its commit.
    fn leave_active_document(&mut self) {
        if let Some(document) = self.active_document()
            && document.surface.is_composing()
        {
            document.surface.ime(ImeEvent::Disabled);
        }
    }

    fn open_untitled(&mut self, kind: DocKind) {
        self.leave_active_document();
        let strings = self.language.strings();
        let id = self.tabs.open_untitled(kind, strings.untitled(kind));
        if kind == DocKind::Writer {
            let document = Document::new(strings.writer_sample, &self.tokens);
            self.documents.insert(id, document);
        }
    }

    fn open_recent(&mut self, index: usize) {
        let Some(file) = RECENT_FILES.get(index) else {
            return;
        };
        self.leave_active_document();
        if self.tabs.activate_existing(file.kind, file.title).is_some() {
            return;
        }
        let id = self.tabs.open(file.kind, file.title);
        if file.kind == DocKind::Writer {
            let text = i18n::fill(self.language.strings().sample_document, "title", file.title);
            self.documents
                .insert(id, Document::new(&text, &self.tokens));
        }
    }

    fn activate(&mut self, index: usize) {
        if let Some(id) = self.tabs.tabs().get(index).map(|tab| tab.id) {
            self.leave_active_document();
            self.tabs.activate(id);
        }
    }

    fn close(&mut self, index: usize) {
        if let Some(id) = self.tabs.tabs().get(index).map(|tab| tab.id) {
            self.tabs.close(id);
            self.documents.remove(&id);
        }
    }

    fn show_start_page(&mut self) {
        self.leave_active_document();
        self.tabs.show_start_page();
    }

    fn set_theme(&mut self, mode: ThemeMode) {
        self.tokens = theme::Tokens::of(mode);
        let (palette, page) = (palette(&self.tokens), page_layout(&self.tokens));
        for document in self.documents.values_mut() {
            document.surface.set_palette(palette);
            document.surface.set_page_layout(page);
        }
    }

    /// Carry out a toolbar or status-bar command. Returns whether anything changed.
    fn run(&mut self, command: Command) -> bool {
        let Some(kind) = self.tabs.active().map(|tab| tab.kind) else {
            return false;
        };
        if !command.is_available(kind) {
            return false;
        }
        let Some(document) = self.active_document() else {
            return false;
        };
        let percent = match command {
            Command::ZoomIn => zoom_step(document.zoom_percent, true),
            Command::ZoomOut => zoom_step(document.zoom_percent, false),
            Command::ZoomReset => 100,
            _ => return false,
        };
        document.zoom_percent = percent;
        document.surface.set_zoom(percent as f32 / 100.0);
        true
    }
}

fn palette(tokens: &theme::Tokens) -> Palette {
    let c = &tokens.colors;
    Palette {
        desk: c.desk.to_array(),
        page: c.page.to_array(),
        page_edge: c.page_edge.to_array(),
        ink: c.ink.to_array(),
        selection: c.selection.to_array(),
        caret: c.caret.to_array(),
    }
}

fn page_layout(tokens: &theme::Tokens) -> PageLayout {
    let page = &tokens.page;
    PageLayout {
        margin: f64::from(page.desk_margin),
        padding: f64::from(page.padding),
        max_width: Some(f64::from(page.width)),
        min_aspect: Some(f64::from(page.aspect)),
    }
}

fn color(value: Rgba) -> Color {
    Color::from_argb_u8(value.3, value.0, value.1, value.2)
}

fn model<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}

/// Copy the design tokens into the Slint globals.
fn apply_tokens(ui: &AppWindow, tokens: &theme::Tokens) {
    let t = ui.global::<Tokens>();
    let c = &tokens.colors;
    t.set_dark(tokens.mode == ThemeMode::Dark);
    t.set_surface(color(c.surface));
    t.set_sunken(color(c.sunken));
    t.set_panel(color(c.panel));
    t.set_fill(color(c.fill));
    t.set_hover(color(c.hover));
    t.set_pressed(color(c.pressed));
    t.set_hairline(color(c.hairline));
    t.set_text(color(c.text));
    t.set_text_secondary(color(c.text_secondary));
    t.set_text_tertiary(color(c.text_tertiary));
    t.set_text_disabled(color(c.text_disabled));
    t.set_accent(color(c.accent));
    t.set_on_accent(color(c.on_accent));
    t.set_accent_soft(color(c.accent_soft));
    t.set_desk(color(c.desk));
    t.set_page(color(c.page));
    t.set_page_edge(color(c.page_edge));

    let m = &tokens.metrics;
    t.set_space_xs(m.space_xs);
    t.set_space_sm(m.space_sm);
    t.set_space_md(m.space_md);
    t.set_space_lg(m.space_lg);
    t.set_space_xl(m.space_xl);
    t.set_radius_sm(m.radius_sm);
    t.set_radius_md(m.radius_md);
    t.set_radius_lg(m.radius_lg);
    t.set_control_height(m.control_height);
    t.set_tab_strip_height(m.tab_strip_height);
    t.set_toolbar_height(m.toolbar_height);
    t.set_status_bar_height(m.status_bar_height);
    t.set_sidebar_width(m.sidebar_width);
    t.set_icon_size(m.icon_size);
    t.set_font_xs(m.font_xs);
    t.set_font_sm(m.font_sm);
    t.set_font_md(m.font_md);
    t.set_font_lg(m.font_lg);
    t.set_font_xl(m.font_xl);
    t.set_font_display(m.font_display);

    apply_native_theme(ui, tokens.mode);
}

/// Make the native parts of the window (title bar, window buttons) follow the
/// theme. Returns false while the native window does not exist yet.
fn apply_native_theme(ui: &AppWindow, mode: ThemeMode) -> bool {
    let native = match mode {
        ThemeMode::Light => winit::window::Theme::Light,
        ThemeMode::Dark => winit::window::Theme::Dark,
    };
    ui.window()
        .with_winit_window(|window| window.set_theme(Some(native)))
        .is_some()
}

/// Set-up that needs the native window, which Slint only creates when the
/// window is first shown. Returns false until it has been done.
fn prepare_native_window(ui: &AppWindow, mode: ThemeMode) -> bool {
    #[cfg(target_os = "macos")]
    ui.window().with_winit_window(macos::claim_title_bar_clicks);
    apply_native_theme(ui, mode)
}

fn apply_icons(ui: &AppWindow) {
    let icons = ui.global::<Icons>();
    icons.set_close(Icon::Close.path().into());
    icons.set_plus(Icon::Plus.path().into());
    icons.set_minus(Icon::Minus.path().into());
    icons.set_chevron_down(Icon::ChevronDown.path().into());
    icons.set_chevron_right(Icon::ChevronRight.path().into());
    icons.set_sidebar(Icon::Sidebar.path().into());
    icons.set_sun(Icon::Sun.path().into());
    icons.set_moon(Icon::Moon.path().into());
    icons.set_folder(Icon::Folder.path().into());
    icons.set_clock(Icon::Clock.path().into());
    icons.set_search(Icon::Search.path().into());
    icons.set_globe(Icon::Globe.path().into());
    icons.set_assistant(Icon::Assistant.path().into());
    icons.set_check(Icon::Check.path().into());
}

/// Copy the interface text of a language into the Slint globals, and rebuild
/// the lists that carry text.
fn apply_language(ui: &AppWindow, language: Language) {
    let strings = language.strings();
    let s = ui.global::<Strings>();
    s.set_tagline(strings.tagline.into());
    s.set_new_document(strings.new_document.into());
    s.set_recent(strings.recent.into());
    s.set_files(strings.files.into());
    s.set_sample(strings.sample.into());
    s.set_sample_data(strings.sample_data.into());
    s.set_placeholder(strings.placeholder.into());
    s.set_placeholder_view(strings.placeholder_view.into());
    s.set_search_files(strings.search_files.into());
    s.set_language(strings.language.into());
    s.set_placeholder_body(strings.placeholder_body.into());
    s.set_ai_title(strings.ai_title.into());
    s.set_ai_body(strings.ai_body.into());
    s.set_ai_point_tools(strings.ai_point_tools.into());
    s.set_ai_point_approval(strings.ai_point_approval.into());
    s.set_ai_point_keys(strings.ai_point_keys.into());
    s.set_ai_input(strings.ai_input.into());

    ui.set_language_index(language.index() as i32);
    ui.set_kinds(model(
        DocKind::ALL
            .into_iter()
            .map(|kind| KindItem {
                kind: kind.index() as i32,
                name: kind.name().into(),
                tagline: strings.kind_tagline(kind).into(),
                implemented: kind.is_implemented(),
            })
            .collect(),
    ));
    // Japanese text must not be drawn with Simplified Chinese letterforms, nor
    // the other way round.
    host::configure_shell_fonts_for(language.tag());
}

/// Lists that never change while the window is open.
fn apply_static_models(ui: &AppWindow) {
    ui.set_languages(model(
        Language::ALL
            .into_iter()
            .map(|language| SharedString::from(language.native_name()))
            .collect(),
    ));
    ui.set_recent(model(
        RECENT_FILES
            .iter()
            .map(|file| RecentItem {
                title: file.title.into(),
                kind: file.kind.index() as i32,
                location: file.location.into(),
                opened: file.opened.into(),
            })
            .collect(),
    ));
    ui.set_tree(model(
        WORKSPACE_TREE
            .iter()
            .map(|row| TreeItem {
                name: row.name.into(),
                depth: i32::from(row.depth),
                kind: row.kind.map_or(-1, |kind| kind.index() as i32),
                expanded: row.expanded,
            })
            .collect(),
    ));
}

fn tool_item(item: &toolbar::Item) -> ToolItem {
    let mut out = ToolItem {
        enabled: item.enabled,
        command: item.command.map_or(-1, Command::id),
        ..ToolItem::default()
    };
    match &item.control {
        Control::Button(icon) => {
            out.control = 0;
            out.icon = icon.path().into();
        }
        Control::Letter(text, style) => {
            out.control = 1;
            out.text = (*text).into();
            out.bold = style.bold;
            out.italic = style.italic;
            out.underline = style.underline;
            out.strike = style.strike;
        }
        Control::Select { value, width } => {
            out.control = 2;
            out.text = value.into();
            out.width = *width;
        }
        Control::Value { text, width } => {
            out.control = 3;
            out.text = text.into();
            out.width = *width;
        }
        Control::Separator => out.control = 4,
        Control::Spacer => out.control = 5,
    }
    out
}

/// Push everything that depends on which tab is in front.
fn refresh_chrome(ui: &AppWindow, shell: &mut Shell) {
    ui.set_tabs(model(
        shell
            .tabs
            .tabs()
            .iter()
            .map(|tab| TabItem {
                title: tab.title.as_str().into(),
                kind: tab.kind.index() as i32,
            })
            .collect(),
    ));
    ui.set_active_index(shell.tabs.active_index().map_or(-1, |index| index as i32));

    let active = shell.tabs.active().map(|tab| (tab.kind, tab.title.clone()));
    match active {
        Some((kind, title)) => {
            ui.set_active_kind(kind.index() as i32);
            ui.set_active_title(title.into());
            let strings = shell.language.strings();
            ui.set_placeholder_title(
                i18n::fill(strings.placeholder_title, "name", kind.name()).into(),
            );
            ui.set_placeholder_plan(strings.plan(kind).into());
        }
        None => {
            ui.set_active_kind(-1);
            ui.set_active_title(SharedString::new());
        }
    }
    refresh_toolbar(ui, shell);
    refresh_status(ui, shell);
}

fn refresh_toolbar(ui: &AppWindow, shell: &mut Shell) {
    let Some(kind) = shell.tabs.active().map(|tab| tab.kind) else {
        ui.set_toolbar_items(model(Vec::new()));
        return;
    };
    let view = ViewState {
        zoom_percent: shell
            .active_document()
            .map_or(100, |document| document.zoom_percent),
    };
    ui.set_toolbar_items(model(
        toolbar::toolbar(kind, view, shell.language.strings())
            .iter()
            .map(tool_item)
            .collect(),
    ));
}

fn refresh_status(ui: &AppWindow, shell: &mut Shell) {
    let strings = shell.language.strings();
    let kind = shell.tabs.active().map(|tab| tab.kind);
    let ime = shell.ime;
    let document = shell.active_document();
    ui.set_status_kind(
        kind.map_or(strings.start, |kind| strings.kind_label(kind))
            .into(),
    );
    ui.set_status_note(
        match kind {
            Some(DocKind::Writer) => strings.plain_text_note,
            Some(_) => strings.placeholder_view,
            None => "",
        }
        .into(),
    );
    match document {
        Some(document) => {
            let count = |template: &str, value: usize| {
                SharedString::from(i18n::fill(template, "count", &group_digits(value)))
            };
            ui.set_status_words(count(strings.word_count, document.stats.words));
            ui.set_status_chars(count(strings.char_count, document.stats.chars));
            ui.set_status_zoom(format!("{}%", document.zoom_percent).into());
            ui.set_zoom_enabled(true);
            let composing = document.surface.is_composing();
            let state = if composing { ImeState::Composing } else { ime };
            ui.set_status_ime(strings.ime(state).into());
            ui.set_status_composing(composing);
        }
        None => {
            ui.set_status_words(SharedString::new());
            ui.set_status_chars(SharedString::new());
            ui.set_status_zoom(SharedString::new());
            ui.set_zoom_enabled(false);
            ui.set_status_ime(SharedString::new());
            ui.set_status_composing(false);
        }
    }
}

/// Render the document in front, hand the frame to Slint, and move the
/// candidate window.
fn present(ui: &AppWindow, shell: &mut Shell) {
    let scale = ui.window().scale_factor();
    let width = to_px(ui.get_surface_width(), scale);
    let height = to_px(ui.get_surface_height(), scale);
    let focused = ui.get_surface_focused();
    let caret_on = shell.caret_on && focused;
    let Some(document) = shell.active_document() else {
        return;
    };
    if width == 0 || height == 0 {
        return;
    }
    document.surface.resize(width, height, scale);
    document.surface.set_caret_on(caret_on);
    if let Some(frame) = host::render_frame(&mut document.surface) {
        ui.set_frame(frame);
    }
    let (offset, max) = document.surface.scroll_range();
    ui.set_scroll_offset(offset as f32 / scale);
    ui.set_scroll_max(max as f32 / scale);

    if focused {
        let origin = (ui.get_surface_x(), ui.get_surface_y());
        host::place_candidate_window(ui.window(), origin, &mut document.surface);
    }
}

/// After the set of tabs or the tab in front changed.
fn after_tab_change(ui: &AppWindow, shell: &mut Shell) {
    shell.wake_caret();
    refresh_chrome(ui, shell);
    if shell.active_document().is_some() {
        ui.invoke_focus_document();
    }
    present(ui, shell);
}

/// After the text or the view of the document in front changed.
fn after_edit(ui: &AppWindow, shell: &mut Shell, text_changed: bool) {
    shell.wake_caret();
    if text_changed && let Some(document) = shell.active_document() {
        document.stats = TextStats::of(&document.surface.text());
    }
    refresh_status(ui, shell);
    present(ui, shell);
}

/// Shortcuts the shell handles before the document sees the key.
fn shell_shortcut(text: &str, primary: bool) -> Option<Command> {
    if !primary {
        return None;
    }
    match text {
        "=" | "+" => Some(Command::ZoomIn),
        "-" => Some(Command::ZoomOut),
        "0" => Some(Command::ZoomReset),
        _ => None,
    }
}

fn select_backend() -> Result<(), slint::PlatformError> {
    let selector = slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("skia".into());
    // On macOS the tab strip is drawn into the title bar, next to the window
    // buttons. Windows and Linux keep their native title bar above the strip.
    #[cfg(target_os = "macos")]
    let selector = selector.with_winit_window_attributes_hook(|attributes| {
        use winit::platform::macos::WindowAttributesExtMacOS;
        attributes
            .with_titlebar_transparent(true)
            .with_title_hidden(true)
            .with_fullsize_content_view(true)
    });
    selector.select()
}

fn run(options: Options) -> Result<(), Box<dyn std::error::Error>> {
    select_backend()?;
    // Following the system language is not built yet; without `--lang` the
    // shell starts in Simplified Chinese.
    let language = options.language.unwrap_or(Language::ZhHans);
    host::configure_shell_fonts_for(language.tag());

    let ui = AppWindow::new()?;
    let shell = Rc::new(RefCell::new(Shell::new(options.theme, language)));
    {
        let mut shell = shell.borrow_mut();
        for kind in options.open {
            shell.open_untitled(kind);
        }
        apply_tokens(&ui, &shell.tokens);
        apply_icons(&ui);
        apply_static_models(&ui);
        apply_language(&ui, shell.language);
        refresh_chrome(&ui, &mut shell);
    }
    if cfg!(target_os = "macos") {
        ui.set_leading_inset(WINDOW_BUTTONS_INSET);
    }
    ui.set_assistant_open(options.assistant);

    // Every handler below follows the same shape: get the window and the shell,
    // change the shell, then push what changed back into the window.
    macro_rules! handler {
        (|$ui:ident, $shell:ident $(, $arg:ident)*| $body:block) => {{
            let shell = shell.clone();
            let weak = ui.as_weak();
            move |$($arg),*| {
                let (Some($ui), Ok(mut $shell)) = (weak.upgrade(), shell.try_borrow_mut()) else {
                    return Default::default();
                };
                let $shell = &mut *$shell;
                $body
            }
        }};
    }

    // Input-method events never reach a Slint FocusScope, so take them from winit.
    ui.window().on_winit_window_event({
        let shell = shell.clone();
        let weak = ui.as_weak();
        move |_window, event| {
            let winit::event::WindowEvent::Ime(ime) = event else {
                return EventResult::Propagate;
            };
            let (Some(ui), Ok(mut shell)) = (weak.upgrade(), shell.try_borrow_mut()) else {
                return EventResult::Propagate;
            };
            let shell = &mut *shell;
            let event = host::ime_event(ime);
            match &event {
                ImeEvent::Enabled => shell.ime = ImeState::Enabled,
                ImeEvent::Disabled => shell.ime = ImeState::Disabled,
                ImeEvent::Preedit { .. } | ImeEvent::Commit(_) => shell.ime = ImeState::Enabled,
            }
            if !ui.get_surface_focused() {
                return EventResult::Propagate;
            }
            let Some(document) = shell.active_document() else {
                return EventResult::Propagate;
            };
            let text_changed = matches!(event, ImeEvent::Commit(_));
            document.surface.ime(event);
            after_edit(&ui, shell, text_changed);
            EventResult::PreventDefault
        }
    });

    ui.on_key_pressed(handler!(|ui, shell, text, shift, primary, alt| {
        if let Some(command) = shell_shortcut(text.as_str(), primary) {
            if shell.run(command) {
                refresh_toolbar(&ui, shell);
                after_edit(&ui, shell, false);
            }
            return true;
        }
        let Some(action) = host::decode_key(text.as_str(), shift, primary, alt) else {
            return false;
        };
        let Some(document) = shell.active_document() else {
            return false;
        };
        document.surface.apply(action);
        after_edit(&ui, shell, true);
        true
    }));

    ui.on_pointer_down(handler!(|ui, shell, x, y, shift| {
        let scale = f64::from(ui.window().scale_factor());
        if let Some(document) = shell.active_document() {
            document
                .surface
                .pointer_down(f64::from(x) * scale, f64::from(y) * scale, shift);
            after_edit(&ui, shell, false);
        }
    }));

    ui.on_pointer_drag(handler!(|ui, shell, x, y| {
        let scale = f64::from(ui.window().scale_factor());
        if let Some(document) = shell.active_document() {
            document
                .surface
                .pointer_drag(f64::from(x) * scale, f64::from(y) * scale);
            present(&ui, shell);
        }
    }));

    ui.on_scrolled(handler!(|ui, shell, delta| {
        let scale = f64::from(ui.window().scale_factor());
        if let Some(document) = shell.active_document()
            && document.surface.scroll_by(-f64::from(delta) * scale)
        {
            present(&ui, shell);
        }
    }));

    ui.on_tab_activate(handler!(|ui, shell, index| {
        shell.activate(usize::try_from(index).unwrap_or(usize::MAX));
        after_tab_change(&ui, shell);
    }));

    ui.on_tab_close(handler!(|ui, shell, index| {
        shell.close(usize::try_from(index).unwrap_or(usize::MAX));
        after_tab_change(&ui, shell);
    }));

    ui.on_new_document(handler!(|ui, shell, kind| {
        if let Some(kind) = usize::try_from(kind).ok().and_then(DocKind::from_index) {
            shell.open_untitled(kind);
            after_tab_change(&ui, shell);
        }
    }));

    ui.on_open_recent(handler!(|ui, shell, index| {
        shell.open_recent(usize::try_from(index).unwrap_or(usize::MAX));
        after_tab_change(&ui, shell);
    }));

    ui.on_show_start(handler!(|ui, shell| {
        shell.show_start_page();
        after_tab_change(&ui, shell);
    }));

    ui.on_toggle_theme(handler!(|ui, shell| {
        shell.set_theme(shell.tokens.mode.toggled());
        apply_tokens(&ui, &shell.tokens);
        present(&ui, shell);
    }));

    ui.on_set_language(handler!(|ui, shell, index| {
        if let Some(language) = usize::try_from(index).ok().and_then(Language::from_index) {
            // Titles and text of documents that are already open stay as they are.
            shell.language = language;
            apply_language(&ui, language);
            refresh_chrome(&ui, shell);
        }
    }));

    ui.on_toolbar_command(handler!(|ui, shell, id| {
        if let Some(command) = Command::from_id(id)
            && shell.run(command)
        {
            refresh_toolbar(&ui, shell);
            after_edit(&ui, shell, false);
        }
    }));

    for (command, register) in [
        (
            Command::ZoomOut,
            AppWindow::on_zoom_out as fn(&AppWindow, Box<dyn FnMut()>),
        ),
        (Command::ZoomReset, AppWindow::on_zoom_reset),
        (Command::ZoomIn, AppWindow::on_zoom_in),
    ] {
        register(
            &ui,
            Box::new(handler!(|ui, shell| {
                if shell.run(command) {
                    refresh_toolbar(&ui, shell);
                    after_edit(&ui, shell, false);
                }
            })),
        );
    }

    ui.on_drag_window({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.window().with_winit_window(|window| {
                    // Fails when the platform cannot start a drag from here;
                    // the native title bar, where there is one, still works.
                    let _ = window.drag_window();
                });
            }
        }
    });

    ui.on_toggle_maximize({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                let window = ui.window();
                window.set_maximized(!window.is_maximized());
            }
        }
    });

    // Picks up resizes, scale changes and focus changes, and blinks the caret.
    let ticker = Timer::default();
    ticker.start(TimerMode::Repeated, Duration::from_millis(33), {
        let mut was_focused = false;
        let mut was_fullscreen = false;
        let mut native_ready = false;
        handler!(|ui, shell| {
            if !native_ready {
                native_ready = prepare_native_window(&ui, shell.tokens.mode);
            }
            let fullscreen = ui.window().is_fullscreen();
            if fullscreen != was_fullscreen && cfg!(target_os = "macos") {
                // A full-screen window has no window buttons to make room for.
                was_fullscreen = fullscreen;
                ui.set_leading_inset(if fullscreen {
                    0.0
                } else {
                    WINDOW_BUTTONS_INSET
                });
            }

            let scale = ui.window().scale_factor();
            let size = (
                to_px(ui.get_surface_width(), scale),
                to_px(ui.get_surface_height(), scale),
                scale,
            );
            let focused = ui.get_surface_focused();
            let blink_due = shell.last_blink.elapsed() >= CARET_BLINK;
            let caret_on = shell.caret_on;
            let Some(document) = shell.active_document() else {
                return;
            };
            let mut dirty = size != document.surface.size() || focused != was_focused;
            was_focused = focused;
            if blink_due {
                shell.last_blink = Instant::now();
                shell.caret_on = !caret_on;
                dirty = dirty || focused;
            }
            if dirty {
                present(&ui, shell);
            }
        })
    });

    ui.run()?;
    Ok(())
}

fn main() -> ExitCode {
    let options = match cli::parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(err) => {
            eprintln!("aster-shell: {err}\n{}", cli::USAGE);
            return ExitCode::FAILURE;
        }
    };
    match run(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("aster-shell: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell() -> Shell {
        Shell::new(ThemeMode::Light, Language::ZhHans)
    }

    #[test]
    fn only_writer_tabs_get_a_document() {
        let mut shell = shell();
        shell.open_untitled(DocKind::Sheets);
        assert!(shell.active_document().is_none());
        shell.open_untitled(DocKind::Writer);
        assert!(shell.active_document().is_some());
        shell.show_start_page();
        assert!(shell.active_document().is_none());
        assert_eq!(shell.documents.len(), 1);
    }

    #[test]
    fn closing_a_tab_drops_its_document() {
        let mut shell = shell();
        shell.open_untitled(DocKind::Writer);
        shell.open_untitled(DocKind::Writer);
        shell.close(0);
        assert_eq!(shell.documents.len(), 1);
        assert!(shell.active_document().is_some());
        shell.close(0);
        assert!(shell.documents.is_empty());
        assert!(shell.tabs.showing_start_page());
        // Out of range: nothing happens.
        shell.close(5);
    }

    #[test]
    fn reopening_a_recent_file_reuses_its_tab() {
        let mut shell = shell();
        shell.open_recent(0);
        shell.open_untitled(DocKind::Slides);
        shell.open_recent(0);
        assert_eq!(shell.tabs.len(), 2);
        assert_eq!(shell.tabs.active_index(), Some(0));
        shell.open_recent(RECENT_FILES.len());
        assert_eq!(shell.tabs.len(), 2);
    }

    #[test]
    fn switching_tabs_abandons_a_composition() {
        let mut shell = shell();
        shell.open_untitled(DocKind::Writer);
        let first = shell.tabs.active_id().expect("a tab is open");
        let before = shell.documents[&first].surface.text();
        shell
            .active_document()
            .expect("writer tab")
            .surface
            .ime(ImeEvent::Preedit {
                text: "nihao".to_owned(),
                cursor: None,
            });
        shell.open_untitled(DocKind::Writer);
        let document = &shell.documents[&first];
        assert!(!document.surface.is_composing());
        assert_eq!(document.surface.text(), before);
    }

    #[test]
    fn new_documents_are_named_in_the_interface_language() {
        let mut shell = shell();
        shell.open_untitled(DocKind::Writer);
        shell.language = Language::En;
        shell.open_untitled(DocKind::Writer);
        shell.language = Language::Ja;
        shell.open_untitled(DocKind::Sheets);
        let titles: Vec<_> = shell
            .tabs
            .tabs()
            .iter()
            .map(|tab| tab.title.as_str())
            .collect();
        assert_eq!(
            titles,
            ["未命名文档 1", "Untitled document 2", "無題のシート 1"]
        );
        // The text of a new document follows the language too.
        let english = shell.tabs.tabs()[1].id;
        assert!(
            shell.documents[&english]
                .surface
                .text()
                .starts_with("Aster Office shell")
        );
    }

    #[test]
    fn zoom_commands_only_act_on_a_writer_tab() {
        let mut shell = shell();
        assert!(!shell.run(Command::ZoomIn));
        shell.open_untitled(DocKind::Sheets);
        assert!(!shell.run(Command::ZoomIn));
        shell.open_untitled(DocKind::Writer);
        assert!(shell.run(Command::ZoomIn));
        assert!(shell.run(Command::ZoomIn));
        let document = shell.active_document().expect("writer tab");
        assert_eq!(document.zoom_percent, 125);
        assert_eq!(document.surface.zoom(), 1.25);
        assert!(shell.run(Command::ZoomReset));
        assert_eq!(
            shell.active_document().expect("writer tab").zoom_percent,
            100
        );
        // Commands that are not built yet change nothing.
        assert!(!shell.run(Command::Bold));
    }

    #[test]
    fn zoom_is_kept_per_document() {
        let mut shell = shell();
        shell.open_untitled(DocKind::Writer);
        shell.run(Command::ZoomIn);
        shell.open_untitled(DocKind::Writer);
        assert_eq!(shell.active_document().expect("second").zoom_percent, 100);
        shell.activate(0);
        assert_eq!(shell.active_document().expect("first").zoom_percent, 110);
    }

    #[test]
    fn shortcuts_need_the_primary_modifier() {
        assert_eq!(shell_shortcut("=", true), Some(Command::ZoomIn));
        assert_eq!(shell_shortcut("-", true), Some(Command::ZoomOut));
        assert_eq!(shell_shortcut("0", true), Some(Command::ZoomReset));
        assert_eq!(shell_shortcut("=", false), None);
        assert_eq!(shell_shortcut("a", true), None);
    }

    #[test]
    fn the_document_palette_comes_from_the_tokens() {
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            let tokens = theme::Tokens::of(mode);
            let palette = palette(&tokens);
            assert_eq!(palette.page, tokens.colors.page.to_array());
            assert_eq!(palette.caret, tokens.colors.caret.to_array());
        }
        let page = page_layout(&theme::Tokens::of(ThemeMode::Light));
        assert_eq!(page.max_width, Some(794.0));
    }

    #[test]
    fn every_control_maps_to_a_ui_item() {
        for kind in DocKind::ALL {
            for item in toolbar::toolbar(kind, ViewState::default(), &i18n::ZH_HANS) {
                let ui_item = tool_item(&item);
                assert!((0..=5).contains(&ui_item.control));
                assert_eq!(ui_item.enabled, item.enabled);
                if let Some(command) = item.command {
                    assert_eq!(Command::from_id(ui_item.command), Some(command));
                } else {
                    assert_eq!(ui_item.command, -1);
                }
            }
        }
    }
}
