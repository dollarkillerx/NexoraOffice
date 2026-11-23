//! Glue between a Slint window and a [`TextSurface`].
//!
//! Slint 1.19 has no input-method API for custom items, so composition events are
//! taken from winit through Slint's `unstable-winit-030` window-event hook, and
//! the candidate window is positioned by calling winit directly.

use slint::platform::Key;
use slint::winit_030::{WinitWindowAccessor, winit};
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

use crate::Surface;
use crate::text_surface::{Action, HAN_FALLBACK_FAMILIES, ImeEvent};

/// Convert a logical length to physical pixels for a buffer dimension.
pub fn to_px(logical: f32, scale: f32) -> u16 {
    (logical * scale).round().clamp(0.0, f32::from(u16::MAX)) as u16
}

/// Detach an input-method event from winit.
pub fn ime_event(ime: &winit::event::Ime) -> ImeEvent {
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
pub fn decode_key(text: &str, shift: bool, primary: bool, alt: bool) -> Option<Action> {
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

/// Give Slint's own text the same Han fallback chain as the document surface.
/// Slint 1.19 lays out text with its own copy of fontique and hits the same
/// single-cached-fallback problem: without this, Simplified-only characters in
/// the shell show as missing glyphs on a Japanese-language Mac.
///
/// Call it after selecting the backend and before creating the first window.
pub fn configure_shell_fonts() {
    configure_shell_fonts_for("zh-Hans");
}

/// Like [`configure_shell_fonts`], for an interface in the given language (a
/// BCP 47 tag). A Japanese interface tries the Japanese face first, so shared
/// Han characters get Japanese letterforms; characters that face lacks still
/// fall through to the rest of the chain. Text that is already on screen may
/// keep its old face until it is laid out again.
pub fn configure_shell_fonts_for(language: &str) {
    use slint::fontique_012::fontique::{FallbackKey, Script};

    let mut collection = slint::fontique_012::shared_collection();
    let families: Vec<_> = han_fallback_order(language)
        .into_iter()
        .filter_map(|name| collection.family_id(name))
        .collect();
    if !families.is_empty() {
        let han = FallbackKey::new(Script::from_bytes(*b"Hani"), None);
        collection.set_fallbacks(han, families.into_iter());
    }
}

/// Families tried before [`HAN_FALLBACK_FAMILIES`] in a Traditional Chinese
/// interface. They ship with macOS; elsewhere they are absent and skipped.
const TRADITIONAL_FIRST: [&str; 2] = ["Heiti TC", "Songti TC"];

/// [`HAN_FALLBACK_FAMILIES`] in the order that suits an interface language.
fn han_fallback_order(language: &str) -> Vec<&'static str> {
    let language = language.to_ascii_lowercase();
    let mut families = HAN_FALLBACK_FAMILIES.to_vec();
    if language.starts_with("ja") {
        // The Japanese face is the last entry of the default chain.
        families.rotate_right(1);
    } else if language.starts_with("zh-hant") {
        families.splice(0..0, TRADITIONAL_FIRST);
    }
    families
}

/// Render the surface at its current size and wrap the frame for a Slint `Image`.
/// Returns `None` while the surface has no area.
pub fn render_frame(surface: &mut dyn Surface) -> Option<Image> {
    let (width, height, _) = surface.size();
    if width == 0 || height == 0 {
        return None;
    }
    let pixmap = surface.render();
    let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        pixmap.data_as_u8_slice(),
        u32::from(width),
        u32::from(height),
    );
    Some(Image::from_rgba8_premultiplied(buffer))
}

/// Tell the platform where the candidate window belongs. `origin` is the
/// surface's top-left corner inside the window, in logical pixels.
pub fn place_candidate_window(
    window: &slint::Window,
    origin: (f32, f32),
    surface: &mut dyn Surface,
) {
    let scale = f64::from(window.scale_factor());
    let (x, y, w, h) = surface.ime_cursor_area();
    let origin_x = f64::from(origin.0) * scale;
    let origin_y = f64::from(origin.1) * scale;
    window.with_winit_window(|window| {
        // Slint only enables the input method for its own text inputs, and it may
        // switch it off again when one of those loses focus, so re-assert it here.
        window.set_ime_allowed(true);
        window.set_ime_cursor_area(
            winit::dpi::PhysicalPosition::new(origin_x + x, origin_y + y),
            winit::dpi::PhysicalSize::new(w, h),
        );
    });
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

    #[test]
    fn japanese_interfaces_try_the_japanese_face_first() {
        assert_eq!(han_fallback_order("zh-Hans"), HAN_FALLBACK_FAMILIES);
        assert_eq!(han_fallback_order("en"), HAN_FALLBACK_FAMILIES);
        let japanese = han_fallback_order("ja-JP");
        assert_eq!(japanese[0], "Hiragino Sans");
        assert_eq!(japanese.len(), HAN_FALLBACK_FAMILIES.len());
        assert_eq!(japanese[1..], HAN_FALLBACK_FAMILIES[..3]);
        let traditional = han_fallback_order("zh-Hant");
        assert_eq!(traditional[..2], TRADITIONAL_FIRST);
        assert_eq!(traditional[2..], HAN_FALLBACK_FAMILIES);
    }

    #[test]
    fn logical_lengths_round_to_whole_pixels() {
        assert_eq!(to_px(100.4, 2.0), 201);
        assert_eq!(to_px(-5.0, 2.0), 0);
        assert_eq!(to_px(1.0e9, 2.0), u16::MAX);
    }
}
