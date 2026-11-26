//! Characters of the symbol fonts Word uses for bullets and marks, as
//! Unicode. Those fonts put their glyphs at the private-use code points
//! `U+F020` to `U+F0FF` (or at the plain byte values).

/// Fonts whose characters are looked up here.
pub fn is_symbol_font(name: &str) -> bool {
    let name = name.trim();
    ["Symbol", "Wingdings", "Wingdings 2", "Wingdings 3", "Webdings"]
        .iter()
        .any(|font| name.eq_ignore_ascii_case(font))
}

/// The Unicode character that `c` in `font` stands for, when the table
/// knows it.
pub fn to_unicode(font: &str, c: char) -> Option<char> {
    let code = c as u32;
    let byte = match code {
        0xF020..=0xF0FF => code - 0xF000,
        0x20..=0xFF => code,
        _ => return None,
    };
    let table: &[(u32, char)] = if font.trim().eq_ignore_ascii_case("Symbol") {
        &SYMBOL
    } else if font.trim().eq_ignore_ascii_case("Wingdings") {
        &WINGDINGS
    } else {
        return None;
    };
    table
        .iter()
        .find(|(from, _)| *from == byte)
        .map(|(_, to)| *to)
}

const SYMBOL: [(u32, char); 27] = [
    (0xB7, '\u{2022}'),
    (0xA7, '\u{2663}'),
    (0xA8, '\u{2666}'),
    (0xA9, '\u{2665}'),
    (0xAA, '\u{2660}'),
    (0xAB, '\u{2194}'),
    (0xAC, '\u{2190}'),
    (0xAD, '\u{2191}'),
    (0xAE, '\u{2192}'),
    (0xAF, '\u{2193}'),
    (0xB0, '\u{00B0}'),
    (0xB1, '\u{00B1}'),
    (0xB2, '\u{2033}'),
    (0xB3, '\u{2265}'),
    (0xB4, '\u{00D7}'),
    (0xB8, '\u{00F7}'),
    (0xB9, '\u{2260}'),
    (0xA3, '\u{2264}'),
    (0xA5, '\u{221E}'),
    (0xD6, '\u{221A}'),
    (0xDE, '\u{21D2}'),
    (0xDC, '\u{21D0}'),
    (0xDB, '\u{21D4}'),
    (0xD8, '\u{00AC}'),
    (0xC5, '\u{2295}'),
    (0xD7, '\u{22C5}'),
    (0xE0, '\u{25CA}'),
];

const WINGDINGS: [(u32, char); 30] = [
    (0x6C, '\u{25CF}'),
    (0x6E, '\u{25A0}'),
    (0x6F, '\u{25A1}'),
    (0x70, '\u{2750}'),
    (0x71, '\u{2751}'),
    (0x75, '\u{25C6}'),
    (0x76, '\u{2756}'),
    (0x77, '\u{2B27}'),
    (0x78, '\u{2612}'),
    (0xA7, '\u{25AA}'),
    (0xA8, '\u{25FB}'),
    (0xD8, '\u{27A2}'),
    (0xE0, '\u{2794}'),
    (0xE8, '\u{2794}'),
    (0xF0, '\u{21E8}'),
    (0xFB, '\u{2717}'),
    (0xFC, '\u{2713}'),
    (0xFD, '\u{2612}'),
    (0xFE, '\u{2611}'),
    (0x4A, '\u{263A}'),
    (0x4C, '\u{2639}'),
    (0x21, '\u{270F}'),
    (0x22, '\u{2702}'),
    (0x3C, '\u{2606}'),
    (0x53, '\u{2600}'),
    (0x54, '\u{2744}'),
    (0xAB, '\u{2B50}'),
    (0xA4, '\u{25CB}'),
    (0xA1, '\u{25CB}'),
    (0xA2, '\u{25CF}'),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_bullets_become_unicode() {
        assert_eq!(to_unicode("Symbol", '\u{F0B7}'), Some('•'));
        assert_eq!(to_unicode("Wingdings", '\u{F0A7}'), Some('▪'));
        assert_eq!(to_unicode("Wingdings", '\u{F0FC}'), Some('✓'));
        assert_eq!(to_unicode("Wingdings", 'l'), Some('●'));
        assert_eq!(to_unicode("Calibri", '\u{F0B7}'), None);
        assert!(is_symbol_font("wingdings"));
    }
}
