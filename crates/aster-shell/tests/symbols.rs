//! Every character the program writes by itself must be in some font the
//! fallback finds; otherwise it is drawn as the system font's missing glyph,
//! a box of bars, as ↩ was in the first footnote back links. This covers the
//! interface text in every language, the preview's own marks and the labels
//! drawn as text in the chrome. Slint draws the chrome's text with its own
//! fallback, which may find more; this checks the stricter one, the one the
//! documents use.
//!
//! The fonts are the system's, so this checks the system it runs on. Only
//! macOS has run it so far.

use aster_shell::i18n::Language;
use aster_text::{Align, Family, Rules, Shaper, Spec, Style};

/// Marks the program puts into text by itself, outside the string tables.
const MARKS: &[&str] = &[
    // The preview.
    aster_surface::preview::BULLET,
    aster_markdown::preview::FOOTNOTE_BACK,
    "1. 2. 10. 123",
    // Labels drawn as text in the chrome: toolbar letters, find options.
    "B I U S Aa ab .*",
    // Separators the shell joins status bar items with.
    "·…—",
];

fn shaper() -> Shaper {
    let mut shaper = Shaper::new();
    shaper.set_han_fallbacks(&aster_surface::text_surface::HAN_FALLBACK_FAMILIES);
    shaper
}

fn undrawable(shaper: &mut Shaper, text: &str, language: &str) -> Vec<String> {
    let spec = Spec {
        text,
        base: Style {
            family: Family::SystemUi,
            size: 15.0,
            color: [0, 0, 0, 255],
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            rise: 0.0,
        },
        spans: &[],
        language,
        width: None,
        scale: 2.0,
        line_height: 1.6,
        align: Align::Start,
        rules: Rules::for_language(language),
    };
    shaper
        .lay_out(&spec)
        .undrawable()
        .into_iter()
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_programs_own_marks_can_be_drawn() {
    let mut shaper = shaper();
    for marks in MARKS {
        let bad = undrawable(&mut shaper, marks, "zh-Hans");
        assert!(bad.is_empty(), "{marks:?}: {bad:?}");
    }
}

#[test]
fn interface_text_in_every_language_can_be_drawn() {
    let mut shaper = shaper();
    for language in Language::ALL {
        for (name, text) in language.strings().fields() {
            let bad = undrawable(&mut shaper, text, language.tag());
            assert!(bad.is_empty(), "{language:?} {name}: {bad:?}");
        }
    }
}
