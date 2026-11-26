//! Font names documents use, mapped to what a machine has.
//!
//! Documents name fonts that are not on every system (SimSun, Calibri, MS
//! Mincho). No fonts are bundled, so each name is followed in the family
//! list by the closest fonts macOS ships; Parley takes the first family that
//! has the glyph. Where nothing in the list exists the system font is used.

use std::sync::Arc;

use aster_text::Family;
use aster_writer::RunLook;

/// What to try, in order, instead of the font `name`. Names are compared
/// ignoring case, spaces and full-width forms.
pub fn substitutes(name: &str) -> &'static [&'static str] {
    let key = fold(name);
    match key.as_str() {
        "宋体" | "simsun" | "新宋体" | "nsimsun" | "simsunextb" | "宋体-简" => {
            &["Songti SC", "STSong", "Hiragino Mincho ProN"]
        }
        "黑体" | "simhei" | "黑体-简" => &["Heiti SC", "PingFang SC", "Hiragino Sans GB"],
        "微软雅黑" | "microsoftyahei" | "微软雅黑light" | "microsoftyaheiui" | "等线"
        | "dengxian" => &["PingFang SC", "Heiti SC"],
        "楷体" | "kaiti" | "楷体_gb2312" | "kaiti_gb2312" | "标楷体" | "dfkaisb" => {
            &["Kaiti SC", "STKaiti"]
        }
        "仿宋" | "fangsong" | "仿宋_gb2312" | "fangsong_gb2312" => &["STFangsong", "Songti SC"],
        "细明体" | "pmingliu" | "mingliu" | "新細明體" | "新细明体" => {
            &["Songti TC", "Songti SC", "PingFang TC"]
        }
        "微軟正黑體" | "microsoftjhenghei" => &["PingFang TC", "PingFang SC"],
        "ms明朝" | "msmincho" | "msp明朝" | "mspmincho" | "游明朝" | "yumincho" => {
            &["Hiragino Mincho ProN", "Hiragino Mincho Pro"]
        }
        "msゴシック" | "msgothic" | "mspゴシック" | "mspgothic" | "游ゴシック" | "yugothic"
        | "meiryo" | "メイリオ" | "meiryoui" | "msuigothic" => {
            &["Hiragino Sans", "Hiragino Kaku Gothic ProN"]
        }
        "malgungothic" | "맑은고딕" | "batang" | "gulim" | "dotum" => {
            &["Apple SD Gothic Neo", "AppleGothic"]
        }
        "calibri" | "calibrilight" | "segoeui" | "tahoma" | "verdana" | "trebuchetms"
        | "candara" | "corbel" | "gillsans" => &["Helvetica Neue", "Helvetica"],
        "cambria" | "georgia" | "constantia" | "garamond" | "bookantiqua" | "palatinolinotype" => {
            &["Georgia", "Times New Roman", "Times"]
        }
        "timesnewroman" | "times" => &["Times New Roman", "Times"],
        "arial" | "arialnarrow" | "arialunicodems" | "helvetica" => &["Arial", "Helvetica Neue"],
        "couriernew" | "courier" | "consolas" | "lucidaconsole" | "lucidasanstypewriter" => {
            &["Courier New", "Menlo", "Courier"]
        }
        "symbol" | "wingdings" | "webdings" => &["Apple Symbols", "Menlo"],
        _ => &[],
    }
}

/// Lower case, without spaces, with full-width Latin letters made ordinary.
fn fold(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| match c {
            '\u{ff01}'..='\u{ff5e}' => char::from_u32(c as u32 - 0xff01 + 0x21).unwrap_or(c),
            c => c,
        })
        .flat_map(char::to_lowercase)
        .collect()
}

/// The families for a look: the Latin font, the East Asian font, each
/// followed by what stands in for it, then the system font.
pub fn family(look: &RunLook) -> Family {
    let mut names: Vec<String> = Vec::new();
    for font in [look.font.as_deref(), look.east_asian_font.as_deref()]
        .into_iter()
        .flatten()
    {
        let clean: String = font
            .chars()
            .filter(|c| !matches!(c, '"' | ',' | ';'))
            .collect();
        let clean = clean.trim();
        if clean.is_empty() {
            continue;
        }
        names.push(format!("\"{clean}\""));
        names.extend(substitutes(clean).iter().map(|s| format!("\"{s}\"")));
    }
    if names.is_empty() {
        return Family::SystemUi;
    }
    Family::List(format!("{}, system-ui", names.join(", ")).into())
}

type FontNames = (Option<Arc<str>>, Option<Arc<str>>);

/// A look's family list, shared between looks that name the same fonts.
#[derive(Default)]
pub struct Families {
    cache: std::collections::HashMap<FontNames, Family>,
}

impl Families {
    pub fn get(&mut self, look: &RunLook) -> Family {
        let key = (look.font.clone(), look.east_asian_font.clone());
        self.cache
            .entry(key)
            .or_insert_with(|| family(look))
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn office_fonts_map_to_system_fonts() {
        assert_eq!(substitutes("宋体")[0], "Songti SC");
        assert_eq!(substitutes("SimSun")[0], "Songti SC");
        assert_eq!(substitutes("Microsoft YaHei")[0], "PingFang SC");
        assert_eq!(substitutes("ＭＳ 明朝")[0], "Hiragino Mincho ProN");
        assert_eq!(substitutes("MS Gothic")[0], "Hiragino Sans");
        assert_eq!(substitutes("Calibri")[0], "Helvetica Neue");
        assert!(substitutes("Some Unknown Font").is_empty());
    }

    #[test]
    fn a_family_list_names_the_font_first() {
        let look = RunLook {
            font: Some("Calibri".into()),
            east_asian_font: Some("宋体".into()),
            ..RunLook::default()
        };
        let Family::List(list) = family(&look) else {
            panic!("a list");
        };
        assert!(list.starts_with("\"Calibri\", \"Helvetica Neue\""));
        assert!(list.contains("\"宋体\", \"Songti SC\""));
        assert!(list.ends_with("system-ui"));
        assert_eq!(family(&RunLook::default()), Family::SystemUi);
    }
}
