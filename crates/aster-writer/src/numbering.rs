//! List labels: counting through the paragraphs of a document and writing
//! the numbers in each level's format.

use std::collections::{HashMap, HashSet};

use crate::{Document, Label, List, ListLevel, NumberFormat, Path};

/// Levels a list has.
const LEVELS: usize = 9;

/// See [`Document::renumber`].
///
/// Lists of the same definition share their counters. An item counts its
/// own level up and starts the deeper levels again. A list's restart
/// number applies the first time the list is used at its level.
pub fn renumber(document: &mut Document) {
    let lists: HashMap<u32, &List> = document
        .numbering
        .lists
        .iter()
        .map(|list| (list.id, list))
        .collect();
    let fallback = ListLevel::default();
    // Where each definition's levels have got to.
    let mut counters: HashMap<u32, [Option<u32>; LEVELS]> = HashMap::new();
    let mut restarted: HashSet<(u32, usize)> = HashSet::new();
    let mut changes: Vec<(Path, Option<Label>)> = Vec::new();

    for (path, paragraph) in document.paragraphs() {
        let label = paragraph
            .props
            .list
            .and_then(|r| lists.get(&r.list).map(|list| (r, *list)))
            .map(|(r, list)| {
                let level = usize::from(r.level).min(LEVELS - 1);
                let state = counters.entry(list.definition).or_insert([None; LEVELS]);
                let own = list.levels.get(level).unwrap_or(&fallback);
                let number = match own.restart {
                    Some(restart) if restarted.insert((list.id, level)) => restart,
                    _ => state[level].map_or(own.start, |n| n.saturating_add(1)),
                };
                state[level] = Some(number);
                for deeper in &mut state[level + 1..] {
                    *deeper = None;
                }
                let mut look = paragraph.mark.clone();
                own.look.apply(&mut look);
                Label {
                    text: label_text(list, level, number, state),
                    look,
                    suffix: own.suffix,
                    align: own.align,
                }
            });
        if paragraph.label != label {
            changes.push((path, label));
        }
    }
    for (path, label) in changes {
        if let Some(paragraph) = document.paragraph_mut(&path) {
            paragraph.label = label;
        }
    }
}

/// The level's text with `%1` to `%9` replaced by the numbers of those
/// levels.
fn label_text(list: &List, level: usize, number: u32, state: &[Option<u32>; LEVELS]) -> String {
    let own = &list.levels[level.min(list.levels.len().saturating_sub(1))];
    let own = list.levels.get(level).unwrap_or(own);
    if own.format == NumberFormat::Bullet {
        return own.text.clone();
    }
    let mut out = String::new();
    let mut chars = own.text.chars().peekable();
    while let Some(c) = chars.next() {
        let digit = match (c, chars.peek()) {
            ('%', Some(d)) => d.to_digit(10).filter(|d| (1..=9).contains(d)),
            _ => None,
        };
        let Some(digit) = digit else {
            out.push(c);
            continue;
        };
        chars.next();
        let at = digit as usize - 1;
        let value = if at == level {
            number
        } else {
            let start = list.levels.get(at).map_or(1, |l| l.start);
            state[at].unwrap_or(start)
        };
        let format = if own.legal {
            &NumberFormat::Decimal
        } else {
            list.levels.get(at).map_or(&NumberFormat::Decimal, |l| &l.format)
        };
        out.push_str(&format_number(value, format));
    }
    out
}

/// `n` written in `format`: `3` as `3`, `c`, `iii`, `③`, `三`. Formats
/// without numbers (bullets, none) give an empty string.
pub fn format_number(n: u32, format: &NumberFormat) -> String {
    match format {
        NumberFormat::Bullet | NumberFormat::None => String::new(),
        NumberFormat::Decimal | NumberFormat::Other(_) => n.to_string(),
        NumberFormat::DecimalZero => format!("{n:02}"),
        NumberFormat::LowerLetter => letters(n, b'a'),
        NumberFormat::UpperLetter => letters(n, b'A'),
        NumberFormat::LowerRoman => roman(n).map_or_else(|| n.to_string(), |r| r.to_lowercase()),
        NumberFormat::UpperRoman => roman(n).unwrap_or_else(|| n.to_string()),
        NumberFormat::DecimalEnclosedCircle => match n {
            1..=20 => char::from_u32(0x2460 + n - 1).map_or_else(|| n.to_string(), String::from),
            _ => n.to_string(),
        },
        NumberFormat::DecimalFullWidth => n
            .to_string()
            .chars()
            .filter_map(|d| d.to_digit(10).and_then(|d| char::from_u32(0xFF10 + d)))
            .collect(),
        NumberFormat::ChineseCounting => chinese(n, &CHINESE, true),
        NumberFormat::ChineseLegal => chinese(n, &LEGAL, false),
        NumberFormat::IdeographDigital => n
            .to_string()
            .chars()
            .filter_map(|d| d.to_digit(10))
            .map(|d| CHINESE.digits[d as usize])
            .map(|d| if d == '零' { '〇' } else { d })
            .collect(),
        NumberFormat::IdeographTraditional => pick(n, &TRADITIONAL),
        NumberFormat::IdeographZodiac => pick(n, &ZODIAC),
        NumberFormat::Aiueo => pick(n, &AIUEO),
        NumberFormat::Iroha => pick(n, &IROHA),
        NumberFormat::Ordinal => {
            let suffix = match (n % 100, n % 10) {
                (11..=13, _) => "th",
                (_, 1) => "st",
                (_, 2) => "nd",
                (_, 3) => "rd",
                _ => "th",
            };
            format!("{n}{suffix}")
        }
    }
}

/// a, b, … z, aa, bb, … zz, aaa: the letter repeats as the alphabet is gone
/// through again.
fn letters(n: u32, first: u8) -> String {
    if n == 0 {
        return "0".to_owned();
    }
    let letter = char::from(first + ((n - 1) % 26) as u8);
    let repeat = (n - 1) / 26 + 1;
    std::iter::repeat_n(letter, repeat.min(1_000) as usize).collect()
}

fn roman(n: u32) -> Option<String> {
    if !(1..=3_999).contains(&n) {
        return None;
    }
    const PARTS: [(u32, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut left = n;
    let mut out = String::new();
    for (value, text) in PARTS {
        while left >= value {
            out.push_str(text);
            left -= value;
        }
    }
    Some(out)
}

struct Numerals {
    digits: [char; 10],
    ten: char,
    hundred: char,
    thousand: char,
}

const CHINESE: Numerals = Numerals {
    digits: ['零', '一', '二', '三', '四', '五', '六', '七', '八', '九'],
    ten: '十',
    hundred: '百',
    thousand: '千',
};

const LEGAL: Numerals = Numerals {
    digits: ['零', '壹', '贰', '叁', '肆', '伍', '陆', '柒', '捌', '玖'],
    ten: '拾',
    hundred: '佰',
    thousand: '仟',
};

/// Counting in Chinese up to 9,999: 十一, 二十, 一百零一. `bare_ten`
/// writes 10 to 19 as 十, 十一 rather than 一十, 一十一.
fn chinese(n: u32, numerals: &Numerals, bare_ten: bool) -> String {
    if n == 0 {
        return numerals.digits[0].to_string();
    }
    if n > 9_999 {
        return n.to_string();
    }
    let (t, h, te, o) = (
        (n / 1000) as usize,
        (n / 100 % 10) as usize,
        (n / 10 % 10) as usize,
        (n % 10) as usize,
    );
    let mut out = String::new();
    let mut zero = false;
    if t > 0 {
        out.push(numerals.digits[t]);
        out.push(numerals.thousand);
    }
    if h > 0 {
        if zero {
            out.push(numerals.digits[0]);
        }
        out.push(numerals.digits[h]);
        out.push(numerals.hundred);
        zero = false;
    } else if t > 0 && (te > 0 || o > 0) {
        zero = true;
    }
    if te > 0 {
        if zero {
            out.push(numerals.digits[0]);
        }
        if !(te == 1 && out.is_empty() && bare_ten) {
            out.push(numerals.digits[te]);
        }
        out.push(numerals.ten);
        zero = false;
    } else if !out.is_empty() && o > 0 {
        zero = true;
    }
    if o > 0 {
        if zero {
            out.push(numerals.digits[0]);
        }
        out.push(numerals.digits[o]);
    }
    out
}

const TRADITIONAL: [char; 10] = ['甲', '乙', '丙', '丁', '戊', '己', '庚', '辛', '壬', '癸'];
const ZODIAC: [char; 12] = [
    '子', '丑', '寅', '卯', '辰', '巳', '午', '未', '申', '酉', '戌', '亥',
];
const AIUEO: [char; 46] = [
    'ア', 'イ', 'ウ', 'エ', 'オ', 'カ', 'キ', 'ク', 'ケ', 'コ', 'サ', 'シ', 'ス', 'セ', 'ソ', 'タ',
    'チ', 'ツ', 'テ', 'ト', 'ナ', 'ニ', 'ヌ', 'ネ', 'ノ', 'ハ', 'ヒ', 'フ', 'ヘ', 'ホ', 'マ', 'ミ',
    'ム', 'メ', 'モ', 'ヤ', 'ユ', 'ヨ', 'ラ', 'リ', 'ル', 'レ', 'ロ', 'ワ', 'ヲ', 'ン',
];
const IROHA: [char; 47] = [
    'イ', 'ロ', 'ハ', 'ニ', 'ホ', 'ヘ', 'ト', 'チ', 'リ', 'ヌ', 'ル', 'ヲ', 'ワ', 'カ', 'ヨ', 'タ',
    'レ', 'ソ', 'ツ', 'ネ', 'ナ', 'ラ', 'ム', 'ウ', 'ヰ', 'ノ', 'オ', 'ク', 'ヤ', 'マ', 'ケ', 'フ',
    'コ', 'エ', 'テ', 'ア', 'サ', 'キ', 'ユ', 'メ', 'ミ', 'シ', 'ヱ', 'ヒ', 'モ', 'セ', 'ス',
];

/// The `n`th of a fixed series; past its end, decimal.
fn pick(n: u32, series: &[char]) -> String {
    match n.checked_sub(1).and_then(|i| series.get(i as usize)) {
        Some(c) => c.to_string(),
        None => n.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        Block, ListRef, Numbering, Paragraph, ParaProps, Run, RunLook, RunPatch, Story, Suffix,
    };

    fn f(n: u32, format: NumberFormat) -> String {
        format_number(n, &format)
    }

    #[test]
    fn letters_repeat_as_word_does() {
        assert_eq!(f(1, NumberFormat::LowerLetter), "a");
        assert_eq!(f(26, NumberFormat::LowerLetter), "z");
        assert_eq!(f(27, NumberFormat::LowerLetter), "aa");
        assert_eq!(f(28, NumberFormat::LowerLetter), "bb");
        assert_eq!(f(53, NumberFormat::LowerLetter), "aaa");
        assert_eq!(f(2, NumberFormat::UpperLetter), "B");
    }

    #[test]
    fn roman_circled_and_wide() {
        assert_eq!(f(4, NumberFormat::LowerRoman), "iv");
        assert_eq!(f(1994, NumberFormat::UpperRoman), "MCMXCIV");
        assert_eq!(f(4000, NumberFormat::UpperRoman), "4000");
        assert_eq!(f(1, NumberFormat::DecimalEnclosedCircle), "①");
        assert_eq!(f(20, NumberFormat::DecimalEnclosedCircle), "⑳");
        assert_eq!(f(21, NumberFormat::DecimalEnclosedCircle), "21");
        assert_eq!(f(305, NumberFormat::DecimalFullWidth), "３０５");
        assert_eq!(f(7, NumberFormat::DecimalZero), "07");
        assert_eq!(f(12, NumberFormat::DecimalZero), "12");
    }

    #[test]
    fn chinese_counting_and_legal() {
        let counting = |n| f(n, NumberFormat::ChineseCounting);
        assert_eq!(counting(1), "一");
        assert_eq!(counting(10), "十");
        assert_eq!(counting(11), "十一");
        assert_eq!(counting(20), "二十");
        assert_eq!(counting(21), "二十一");
        assert_eq!(counting(99), "九十九");
        assert_eq!(counting(100), "一百");
        assert_eq!(counting(101), "一百零一");
        assert_eq!(counting(110), "一百一十");
        assert_eq!(counting(1010), "一千零一十");
        let legal = |n| f(n, NumberFormat::ChineseLegal);
        assert_eq!(legal(1), "壹");
        assert_eq!(legal(3), "叁");
        assert_eq!(legal(10), "壹拾");
        assert_eq!(legal(12), "壹拾贰");
        assert_eq!(f(10, NumberFormat::IdeographDigital), "一〇");
        assert_eq!(f(2, NumberFormat::IdeographTraditional), "乙");
        assert_eq!(f(11, NumberFormat::IdeographTraditional), "11");
        assert_eq!(f(1, NumberFormat::IdeographZodiac), "子");
        assert_eq!(f(12, NumberFormat::IdeographZodiac), "亥");
        assert_eq!(f(3, NumberFormat::Aiueo), "ウ");
        assert_eq!(f(3, NumberFormat::Iroha), "ハ");
    }

    #[test]
    fn ordinals() {
        let ord = |n| f(n, NumberFormat::Ordinal);
        assert_eq!(ord(1), "1st");
        assert_eq!(ord(2), "2nd");
        assert_eq!(ord(3), "3rd");
        assert_eq!(ord(4), "4th");
        assert_eq!(ord(11), "11th");
        assert_eq!(ord(12), "12th");
        assert_eq!(ord(13), "13th");
        assert_eq!(ord(21), "21st");
        assert_eq!(ord(111), "111th");
        assert_eq!(f(5, NumberFormat::Bullet), "");
        assert_eq!(f(5, NumberFormat::Other("x".into())), "5");
    }

    fn level(format: NumberFormat, text: &str) -> ListLevel {
        ListLevel {
            format,
            text: text.to_owned(),
            start: 1,
            ..ListLevel::default()
        }
    }

    fn item(list: u32, level: u8) -> Block {
        Block::paragraph(Paragraph {
            runs: vec![Run::text("x", RunLook::default())],
            props: ParaProps {
                list: Some(ListRef { list, level }),
                ..ParaProps::default()
            },
            ..Paragraph::default()
        })
    }

    fn plain() -> Block {
        Block::paragraph(Paragraph::default())
    }

    fn labels(document: &Document) -> Vec<Option<String>> {
        document
            .paragraphs()
            .iter()
            .map(|(_, p)| p.label.as_ref().map(|l| l.text.clone()))
            .collect()
    }

    fn document(lists: Vec<List>, blocks: Vec<Block>) -> Document {
        Document {
            body: Story { blocks, part: None },
            numbering: Arc::new(Numbering { lists }),
            ..Document::default()
        }
    }

    fn outline() -> List {
        List {
            id: 1,
            definition: 10,
            levels: vec![
                level(NumberFormat::Decimal, "%1."),
                level(NumberFormat::Decimal, "%1.%2"),
                level(NumberFormat::LowerLetter, "(%3)"),
            ],
        }
    }

    #[test]
    fn deeper_levels_start_again() {
        let mut d = document(
            vec![outline()],
            vec![
                item(1, 0),
                item(1, 1),
                item(1, 1),
                item(1, 2),
                item(1, 0),
                item(1, 1),
                plain(),
                item(1, 0),
            ],
        );
        d.renumber();
        assert_eq!(
            labels(&d),
            ["1.", "1.1", "1.2", "(a)", "2.", "2.1", "", "3."]
                .map(|s| (!s.is_empty()).then(|| s.to_owned()))
        );
    }

    #[test]
    fn lists_of_a_definition_continue_each_other_until_a_restart() {
        let second = List {
            id: 2,
            definition: 10,
            levels: vec![ListLevel {
                restart: Some(5),
                ..level(NumberFormat::Decimal, "%1)")
            }],
        };
        let third = List {
            id: 3,
            definition: 10,
            levels: vec![level(NumberFormat::Decimal, "%1:")],
        };
        let mut d = document(
            vec![outline(), second, third],
            vec![item(1, 0), item(1, 0), item(3, 0), item(2, 0), item(2, 0)],
        );
        d.renumber();
        assert_eq!(
            labels(&d),
            ["1.", "2.", "3:", "5)", "6)"].map(|s| Some(s.to_owned()))
        );
    }

    #[test]
    fn legal_numbering_shows_decimal() {
        let list = List {
            id: 1,
            definition: 1,
            levels: vec![
                level(NumberFormat::UpperRoman, "%1."),
                ListLevel {
                    legal: true,
                    ..level(NumberFormat::LowerLetter, "%1.%2")
                },
            ],
        };
        let mut d = document(vec![list], vec![item(1, 0), item(1, 0), item(1, 1)]);
        d.renumber();
        assert_eq!(
            labels(&d),
            ["I.", "II.", "2.1"].map(|s| Some(s.to_owned()))
        );
    }

    #[test]
    fn bullets_show_their_character_and_the_label_takes_the_level_look() {
        let bullet = List {
            id: 1,
            definition: 1,
            levels: vec![ListLevel {
                look: RunPatch {
                    bold: Some(true),
                    ..RunPatch::default()
                },
                suffix: Suffix::Space,
                ..level(NumberFormat::Bullet, "•")
            }],
        };
        let mut d = document(vec![bullet], vec![item(1, 0)]);
        d.renumber();
        let label = d.paragraphs()[0].1.label.clone().unwrap();
        assert_eq!(label.text, "•");
        assert!(label.look.bold);
        assert_eq!(label.suffix, Suffix::Space);
    }

    #[test]
    fn unchanged_labels_keep_their_arcs() {
        let mut d = document(vec![outline()], vec![item(1, 0), item(1, 0)]);
        d.renumber();
        let before = d.clone();
        d.renumber();
        for (a, b) in d.body.blocks.iter().zip(&before.body.blocks) {
            assert!(a.is(b));
        }
        // An item added at the end changes nothing before it.
        let kept = d.clone();
        d.body.blocks.push(item(1, 0));
        d.renumber();
        assert!(d.body.blocks[0].is(&kept.body.blocks[0]));
        assert!(d.body.blocks[1].is(&kept.body.blocks[1]));
        assert_eq!(labels(&d)[2].as_deref(), Some("3."));
    }

    #[test]
    fn a_list_that_is_gone_leaves_no_label() {
        let mut d = document(vec![outline()], vec![item(1, 0), item(9, 0)]);
        d.renumber();
        assert_eq!(labels(&d), [Some("1.".to_owned()), None]);
        d.paragraph_mut(&Path::body(0)).unwrap().props.list = None;
        d.renumber();
        assert_eq!(labels(&d), [None, None]);
    }
}
