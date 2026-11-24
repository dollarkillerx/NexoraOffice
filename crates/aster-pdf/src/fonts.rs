//! Fonts for text whose font the file does not carry.
//!
//! The 14 standard fonts come with hayro (Foxit's, from PDFium). Chinese,
//! Japanese and Korean text that names a font without embedding it (common in
//! older files: STSong-Light, MS-Mincho, HYSMyeongJo…) is drawn with a font of
//! the system for its character collection.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use hayro::hayro_interpret::font::{FontData, FontQuery};
use hayro::hayro_interpret::hayro_cmap::CidFamily;
use hayro::hayro_interpret::{FontResolverFn, InterpreterSettings};

/// Candidate files for each collection, most common first. A `.ttc` is read
/// at index 0, which is the regular face for all of these.
fn candidates(family: &CidFamily) -> &'static [&'static str] {
    match family {
        CidFamily::AdobeGB1 => &[
            "/System/Library/Fonts/Supplemental/Songti.ttc",
            "/System/Library/Fonts/STHeiti Light.ttc",
            "C:\\Windows\\Fonts\\simsun.ttc",
            "C:\\Windows\\Fonts\\msyh.ttc",
            "/usr/share/fonts/opentype/noto/NotoSerifCJK-Regular.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
        ],
        CidFamily::AdobeCNS1 => &[
            "/System/Library/Fonts/Supplemental/Songti.ttc",
            "C:\\Windows\\Fonts\\mingliu.ttc",
            "C:\\Windows\\Fonts\\msjh.ttc",
            "/usr/share/fonts/opentype/noto/NotoSerifCJK-Regular.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        ],
        CidFamily::AdobeJapan1 => &[
            "/System/Library/Fonts/ヒラギノ明朝 ProN.ttc",
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
            "C:\\Windows\\Fonts\\msmincho.ttc",
            "C:\\Windows\\Fonts\\YuGothR.ttc",
            "/usr/share/fonts/opentype/noto/NotoSerifCJK-Regular.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        ],
        CidFamily::AdobeKorea1 => &[
            "/System/Library/Fonts/AppleSDGothicNeo.ttc",
            "/System/Library/Fonts/Supplemental/AppleMyungjo.ttf",
            "C:\\Windows\\Fonts\\batang.ttc",
            "C:\\Windows\\Fonts\\malgun.ttf",
            "/usr/share/fonts/opentype/noto/NotoSerifCJK-Regular.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        ],
        _ => &[],
    }
}

/// The first candidate that can be read, loaded once per collection.
fn system_font(family: &CidFamily) -> Option<(FontData, u32)> {
    static GB1: OnceLock<Option<FontData>> = OnceLock::new();
    static CNS1: OnceLock<Option<FontData>> = OnceLock::new();
    static JAPAN1: OnceLock<Option<FontData>> = OnceLock::new();
    static KOREA1: OnceLock<Option<FontData>> = OnceLock::new();
    let cell = match family {
        CidFamily::AdobeGB1 => &GB1,
        CidFamily::AdobeCNS1 => &CNS1,
        CidFamily::AdobeJapan1 => &JAPAN1,
        CidFamily::AdobeKorea1 => &KOREA1,
        _ => return None,
    };
    let data = cell.get_or_init(|| {
        candidates(family)
            .iter()
            .filter(|path| Path::new(path).is_file())
            .find_map(|path| std::fs::read(path).ok())
            .map(|bytes| Arc::new(bytes) as FontData)
    });
    data.clone().map(|data| (data, 0))
}

/// Interpreter settings with our font fallback.
pub fn settings() -> InterpreterSettings {
    let resolver: FontResolverFn = Arc::new(|query| match query {
        FontQuery::Standard(standard) => Some(standard.get_font_data()),
        FontQuery::Fallback(fallback) => fallback
            .character_collection
            .as_ref()
            .and_then(|collection| system_font(&collection.family))
            .or_else(|| Some(fallback.pick_standard_font().get_font_data())),
    });
    InterpreterSettings {
        font_resolver: resolver,
        ..Default::default()
    }
}
