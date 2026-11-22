//! Phase 0 gate G1: can a document surface we draw ourselves, hosted in a Slint
//! window, take Chinese and Japanese input-method composition?
//!
//! Slint 1.19 has no public input-method API for custom items, so this spike takes
//! the events from winit directly through Slint's `unstable-winit-030` window-event
//! hook, and tells winit where the candidate window belongs.
//!
//! Run it, click the white page, switch to a CJK input source and type. Every
//! input-method event is printed to stderr and summarised in the window.
//!
//! `g1-ime --dump out.png` renders one frame without opening a window.

mod surface;

use std::cell::RefCell;
use std::process::ExitCode;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::platform::Key;
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer, Timer, TimerMode};
use surface::{Action, ImeEvent, Surface};

slint::include_modules!();

const SAMPLE_TEXT: &str = "\
G1 输入法关卡

这块白色区域不是 Slint 控件。文字由 Parley 排版、vello_cpu 渲染，再作为一张图贴进窗口。
请切到中文拼音或日文假名输入法，在下面打字，观察三件事：
1. 预编辑串是否带下划线出现在光标处
2. 候选窗是否跟着光标走
3. 上屏后是否不丢字、不重字

にほんご 日本語 English 123

";

const CARET_BLINK: Duration = Duration::from_millis(530);

struct App {
    surface: Surface,
    started: Instant,
    last_blink: Instant,
    caret_on: bool,
    enabled_seen: bool,
    preedits: u32,
    commits: u32,
}

impl App {
    fn log(&self, line: &str) {
        eprintln!("[{:9.3}s] {line}", self.started.elapsed().as_secs_f64());
    }

    fn summary(&self) -> String {
        format!(
            "输入法：{} · 预编辑 {} 次 · 上屏 {} 次",
            if self.enabled_seen {
                "已启用"
            } else {
                "未启用"
            },
            self.preedits,
            self.commits
        )
    }
}

fn to_px(logical: f32, scale: f32) -> u16 {
    (logical * scale).round().clamp(0.0, f32::from(u16::MAX)) as u16
}

/// Render the surface, hand the frame to Slint, and move the candidate window.
fn present(ui: &AppWindow, app: &mut App) {
    let scale = ui.window().scale_factor();
    let width = to_px(ui.get_surface_width(), scale);
    let height = to_px(ui.get_surface_height(), scale);
    if width == 0 || height == 0 {
        return;
    }
    app.surface.resize(width, height, scale);
    app.surface
        .set_caret_on(app.caret_on && ui.get_surface_focused());

    let pixmap = app.surface.render();
    let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        pixmap.data_as_u8_slice(),
        u32::from(width),
        u32::from(height),
    );
    ui.set_frame(Image::from_rgba8_premultiplied(buffer));
    ui.set_ime_summary(app.summary().into());

    if !ui.get_surface_focused() {
        return;
    }
    let (x, y, w, h) = app.surface.ime_cursor_area();
    let origin_x = f64::from(ui.get_surface_x() * scale);
    let origin_y = f64::from(ui.get_surface_y() * scale);
    ui.window().with_winit_window(|window| {
        // Slint only enables the input method for its own text inputs, and it may
        // switch it off again when one of those loses focus, so re-assert it here.
        window.set_ime_allowed(true);
        window.set_ime_cursor_area(
            winit::dpi::PhysicalPosition::new(origin_x + x, origin_y + y),
            winit::dpi::PhysicalSize::new(w, h),
        );
    });
}

fn describe(event: &ImeEvent) -> String {
    match event {
        ImeEvent::Enabled => "Enabled".to_owned(),
        ImeEvent::Disabled => "Disabled".to_owned(),
        ImeEvent::Commit(text) => format!("Commit {text:?}"),
        ImeEvent::Preedit { text, cursor } => match cursor {
            Some((start, end)) => format!("Preedit {text:?} cursor {start}..{end}"),
            None => format!("Preedit {text:?} cursor hidden"),
        },
    }
}

fn from_winit(ime: &winit::event::Ime) -> ImeEvent {
    match ime {
        winit::event::Ime::Enabled => ImeEvent::Enabled,
        winit::event::Ime::Disabled => ImeEvent::Disabled,
        winit::event::Ime::Commit(text) => ImeEvent::Commit(text.clone()),
        winit::event::Ime::Preedit(text, cursor) => ImeEvent::Preedit {
            text: text.clone(),
            cursor: *cursor,
        },
    }
}

/// Turn a Slint key event into an editing command. `primary` is Cmd on macOS and
/// Ctrl elsewhere (that is how Slint reports `modifiers.control`).
fn decode_key(text: &str, shift: bool, primary: bool, alt: bool) -> Option<Action> {
    let mut chars = text.chars();
    let first = chars.next()?;
    if chars.next().is_none() {
        let is = |key: Key| char::from(key) == first;
        if is(Key::Backspace) {
            return Some(Action::Backspace);
        }
        if is(Key::Delete) {
            return Some(Action::Delete);
        }
        if is(Key::Return) {
            return Some(Action::Newline);
        }
        if is(Key::Tab) {
            return Some(Action::Insert("\t".to_owned()));
        }
        if is(Key::LeftArrow) {
            return Some(if primary {
                Action::LineStart { select: shift }
            } else {
                Action::Left {
                    select: shift,
                    word: alt,
                }
            });
        }
        if is(Key::RightArrow) {
            return Some(if primary {
                Action::LineEnd { select: shift }
            } else {
                Action::Right {
                    select: shift,
                    word: alt,
                }
            });
        }
        if is(Key::UpArrow) {
            return Some(Action::Up { select: shift });
        }
        if is(Key::DownArrow) {
            return Some(Action::Down { select: shift });
        }
        if is(Key::Home) {
            return Some(Action::LineStart { select: shift });
        }
        if is(Key::End) {
            return Some(Action::LineEnd { select: shift });
        }
    }
    if primary {
        return text.eq_ignore_ascii_case("a").then_some(Action::SelectAll);
    }
    // Slint reports non-text keys as control characters or private-use code points.
    let printable = text
        .chars()
        .all(|c| !c.is_control() && !('\u{e000}'..='\u{f8ff}').contains(&c));
    printable.then(|| Action::Insert(text.to_owned()))
}

/// Give Slint's own text (header, footer) the same Han fallback chain as the
/// document surface. Slint 1.19 lays out text with its own copy of fontique and
/// hits the same single-cached-fallback problem: without this, Simplified-only
/// characters in the shell show as missing glyphs on a Japanese-language Mac.
fn configure_shell_fonts() {
    use slint::fontique_012::fontique::{FallbackKey, Script};

    let mut collection = slint::fontique_012::shared_collection();
    let families: Vec<_> = surface::HAN_FALLBACK_FAMILIES
        .iter()
        .filter_map(|name| collection.family_id(name))
        .collect();
    if !families.is_empty() {
        let han = FallbackKey::new(Script::from_bytes(*b"Hani"), None);
        collection.set_fallbacks(han, families.into_iter());
    }
}

fn dump(path: &str) -> Result<(), String> {
    let mut surface = Surface::new(SAMPLE_TEXT);
    surface.resize(1600, 900, 2.0);
    surface.ime(ImeEvent::Preedit {
        text: "ni hao shi jie".to_owned(),
        cursor: Some((14, 14)),
    });
    let png = surface.encode_png()?;
    std::fs::write(path, png).map_err(|err| format!("cannot write {path}: {err}"))
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("skia".into())
        .select()?;
    configure_shell_fonts();

    let ui = AppWindow::new()?;
    let now = Instant::now();
    let app = Rc::new(RefCell::new(App {
        surface: Surface::new(SAMPLE_TEXT),
        started: now,
        last_blink: now,
        caret_on: true,
        enabled_seen: false,
        preedits: 0,
        commits: 0,
    }));

    // Input-method events never reach a Slint FocusScope, so take them from winit.
    ui.window().on_winit_window_event({
        let app = app.clone();
        let ui = ui.as_weak();
        move |_window, event| {
            let winit::event::WindowEvent::Ime(ime) = event else {
                return EventResult::Propagate;
            };
            let Some(ui) = ui.upgrade() else {
                return EventResult::Propagate;
            };
            let Ok(mut app) = app.try_borrow_mut() else {
                return EventResult::Propagate;
            };
            let event = from_winit(ime);
            let line = describe(&event);
            app.log(&format!("IME {line}"));
            match &event {
                ImeEvent::Enabled => app.enabled_seen = true,
                ImeEvent::Disabled => app.enabled_seen = false,
                ImeEvent::Preedit { text, .. } if !text.is_empty() => app.preedits += 1,
                ImeEvent::Commit(_) => app.commits += 1,
                ImeEvent::Preedit { .. } => {}
            }
            app.surface.ime(event);
            app.caret_on = true;
            app.last_blink = Instant::now();
            let composing = if app.surface.is_composing() {
                " · 正在组字"
            } else {
                ""
            };
            ui.set_last_event(format!("最近的输入法事件：{line}{composing}").into());
            present(&ui, &mut app);
            EventResult::PreventDefault
        }
    });

    ui.on_key_pressed({
        let app = app.clone();
        let ui = ui.as_weak();
        move |text, shift, primary, alt| {
            let (Some(ui), Ok(mut app)) = (ui.upgrade(), app.try_borrow_mut()) else {
                return false;
            };
            let Some(action) = decode_key(text.as_str(), shift, primary, alt) else {
                return false;
            };
            app.log(&format!("KEY {action:?}"));
            app.surface.apply(action);
            app.caret_on = true;
            app.last_blink = Instant::now();
            present(&ui, &mut app);
            true
        }
    });

    ui.on_pointer_down({
        let app = app.clone();
        let ui = ui.as_weak();
        move |x, y, shift| {
            let (Some(ui), Ok(mut app)) = (ui.upgrade(), app.try_borrow_mut()) else {
                return;
            };
            let scale = f64::from(ui.window().scale_factor());
            app.surface
                .pointer_down(f64::from(x) * scale, f64::from(y) * scale, shift);
            app.caret_on = true;
            app.last_blink = Instant::now();
            present(&ui, &mut app);
        }
    });

    ui.on_pointer_drag({
        let app = app.clone();
        let ui = ui.as_weak();
        move |x, y| {
            let (Some(ui), Ok(mut app)) = (ui.upgrade(), app.try_borrow_mut()) else {
                return;
            };
            let scale = f64::from(ui.window().scale_factor());
            app.surface
                .pointer_drag(f64::from(x) * scale, f64::from(y) * scale);
            present(&ui, &mut app);
        }
    });

    // Picks up resizes, scale changes and focus changes, and blinks the caret.
    let ticker = Timer::default();
    ticker.start(TimerMode::Repeated, Duration::from_millis(33), {
        let app = app.clone();
        let ui = ui.as_weak();
        let mut was_focused = false;
        move || {
            let (Some(ui), Ok(mut app)) = (ui.upgrade(), app.try_borrow_mut()) else {
                return;
            };
            let scale = ui.window().scale_factor();
            let size = (
                to_px(ui.get_surface_width(), scale),
                to_px(ui.get_surface_height(), scale),
                scale,
            );
            let focused = ui.get_surface_focused();
            let mut dirty = size != app.surface.size() || focused != was_focused;
            was_focused = focused;
            if app.last_blink.elapsed() >= CARET_BLINK {
                app.last_blink = Instant::now();
                app.caret_on = !app.caret_on;
                dirty = true;
            }
            if dirty {
                present(&ui, &mut app);
            }
        }
    });

    ui.run()?;
    let app = app.borrow();
    app.log(&format!("final text: {:?}", app.surface.text()));
    Ok(())
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let result = match (args.next().as_deref(), args.next()) {
        (Some("--dump"), Some(path)) => dump(&path).map_err(Into::into),
        (None, _) => run(),
        _ => Err("usage: g1-ime [--dump <file.png>]".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("g1-ime: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: Key) -> String {
        char::from(key).to_string()
    }

    #[test]
    fn plain_text_is_inserted() {
        assert_eq!(
            decode_key("a", false, false, false),
            Some(Action::Insert("a".to_owned()))
        );
        assert_eq!(
            decode_key("你", false, false, false),
            Some(Action::Insert("你".to_owned()))
        );
    }

    #[test]
    fn editing_keys_map_to_commands() {
        assert_eq!(
            decode_key(&key(Key::Backspace), false, false, false),
            Some(Action::Backspace)
        );
        assert_eq!(
            decode_key(&key(Key::Return), false, false, false),
            Some(Action::Newline)
        );
        assert_eq!(
            decode_key(&key(Key::LeftArrow), true, false, true),
            Some(Action::Left {
                select: true,
                word: true
            })
        );
        assert_eq!(
            decode_key(&key(Key::RightArrow), false, true, false),
            Some(Action::LineEnd { select: false })
        );
    }

    #[test]
    fn shortcuts_and_function_keys_do_not_insert_text() {
        assert_eq!(decode_key("a", false, true, false), Some(Action::SelectAll));
        assert_eq!(decode_key("s", false, true, false), None);
        assert_eq!(decode_key(&key(Key::F5), false, false, false), None);
        assert_eq!(decode_key(&key(Key::Escape), false, false, false), None);
        assert_eq!(decode_key("", false, false, false), None);
    }
}
