//! What a typed entry means: a number, a logical value, a date or time, a
//! formula, or text, as Excel reads what is typed into a cell.

use std::sync::Arc;

use crate::format::{DateOrder, date_serial};
use crate::model::Value;

/// A cell entry after reading.
#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    /// Nothing: the cell is cleared.
    Empty,
    Value(Value),
    /// Formula text without the `=`, function names and references in
    /// capitals.
    Formula(String),
}

/// What the typed text looked like, so the cell can be given a fitting
/// number format if it has none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    Plain,
    Percent,
    /// With a thousands separator.
    Grouped,
    /// With a currency sign before or after the number.
    Currency {
        symbol: char,
        before: bool,
    },
    Date,
    Time,
    DateTime,
}

/// How to read dates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reading {
    pub order: DateOrder,
    pub date1904: bool,
    /// The year for dates typed without one.
    pub this_year: i64,
}

/// Read what was typed into a cell.
pub fn read(text: &str, reading: &Reading) -> (Entry, Look) {
    if let Some(rest) = text.strip_prefix('\'') {
        return (Entry::Value(Value::Text(Arc::from(rest))), Look::Plain);
    }
    if text.is_empty() {
        return (Entry::Empty, Look::Plain);
    }
    if let Some(formula) = text.strip_prefix('=')
        && !formula.trim().is_empty()
    {
        return (
            Entry::Formula(normalize_formula(formula.trim())),
            Look::Plain,
        );
    }
    let trimmed = text.trim();
    match trimmed.to_ascii_uppercase().as_str() {
        "TRUE" => return (Entry::Value(Value::Bool(true)), Look::Plain),
        "FALSE" => return (Entry::Value(Value::Bool(false)), Look::Plain),
        _ => {}
    }
    if let Some((number, look)) = number(trimmed) {
        return (Entry::Value(Value::Number(number)), look);
    }
    if let Some((serial, look)) = date_time(trimmed, reading) {
        return (Entry::Value(Value::Number(serial)), look);
    }
    (Entry::Value(Value::Text(Arc::from(text))), Look::Plain)
}

const CURRENCY: &[char] = &['$', '¥', '￥', '€', '£', '₩'];

/// `1234.5`, `-1,234`, `(12)`, `12%`, `¥1,200`, `1.5E3`.
fn number(text: &str) -> Option<(f64, Look)> {
    let mut look = Look::Plain;
    let mut s = text.trim();
    let mut negative = false;
    if let Some(inner) = s.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
        negative = true;
        s = inner.trim();
    }
    if let Some(rest) = s.strip_prefix('-') {
        negative = !negative;
        s = rest.trim_start();
    } else if let Some(rest) = s.strip_prefix('+') {
        s = rest.trim_start();
    }
    let mut percent = false;
    if let Some(rest) = s.strip_suffix('%') {
        percent = true;
        s = rest.trim_end();
    }
    if let Some(rest) = s.strip_prefix(CURRENCY) {
        look = Look::Currency {
            symbol: s.chars().next().unwrap_or('$'),
            before: true,
        };
        s = rest.trim_start();
        if let Some(rest) = s.strip_prefix('-') {
            negative = !negative;
            s = rest;
        }
    } else if let Some(rest) = s.strip_suffix(CURRENCY) {
        look = Look::Currency {
            symbol: s.chars().next_back().unwrap_or('$'),
            before: false,
        };
        s = rest.trim_end();
    }
    if s.is_empty() || !s.starts_with(|c: char| c.is_ascii_digit() || c == '.') {
        return None;
    }
    // Thousands separators: groups of three after the first.
    if s.contains(',') {
        let (int, frac) = s.split_once('.').map_or((s, None), |(i, f)| (i, Some(f)));
        let mut groups = int.split(',');
        let first = groups.next()?;
        if first.is_empty() || first.len() > 3 || !groups.all(|g| g.len() == 3) {
            return None;
        }
        if look == Look::Plain {
            look = Look::Grouped;
        }
        let joined = int.replace(',', "");
        let rebuilt = match frac {
            Some(frac) => format!("{joined}.{frac}"),
            None => joined,
        };
        return finish(&rebuilt, negative, percent, look);
    }
    finish(s, negative, percent, look)
}

fn finish(digits: &str, negative: bool, percent: bool, look: Look) -> Option<(f64, Look)> {
    let valid = digits
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'));
    if !valid {
        return None;
    }
    let value: f64 = digits.parse().ok().filter(|v: &f64| v.is_finite())?;
    let value = if negative { -value } else { value };
    if percent {
        Some((value / 100.0, Look::Percent))
    } else {
        Some((value, look))
    }
}

/// Dates (`2026/11/24`, `2026-11-24`, `11/24/2026`, `24.11.2026`,
/// `2026年11月24日`, `11/24` in the current year), times (`9:30`,
/// `21:30:15`, `9:30 PM`) and a date followed by a time.
fn date_time(text: &str, reading: &Reading) -> Option<(f64, Look)> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let joined;
    let (date_part, time_part) = match tokens.as_slice() {
        [one] if one.contains(':') => (None, Some(*one)),
        [one] => (Some(*one), None),
        [clock, meridiem] if is_meridiem(meridiem) => {
            joined = format!("{clock} {meridiem}");
            (None, Some(joined.as_str()))
        }
        [date, clock] if clock.contains(':') => (Some(*date), Some(*clock)),
        [date, clock, meridiem] if is_meridiem(meridiem) => {
            joined = format!("{clock} {meridiem}");
            (Some(*date), Some(joined.as_str()))
        }
        _ => return None,
    };
    let date = match date_part {
        Some(date) => Some(date_serial_of(date, reading)?),
        None => None,
    };
    let time = match time_part {
        Some(time) => Some(time_of(time)?),
        None => None,
    };
    match (date, time) {
        (Some(date), Some(time)) => Some((date + time, Look::DateTime)),
        (Some(date), None) => Some((date, Look::Date)),
        (None, Some(time)) => Some((time, Look::Time)),
        (None, None) => None,
    }
}

fn is_meridiem(text: &str) -> bool {
    matches!(text.to_ascii_uppercase().as_str(), "AM" | "PM" | "A" | "P")
}

fn date_serial_of(text: &str, reading: &Reading) -> Option<f64> {
    let text = text.trim();
    // 2026年11月24日, 11月24日.
    if text.contains('月') {
        let (year, rest) = match text.split_once('年') {
            Some((year, rest)) => (year.trim().parse::<i64>().ok()?, rest),
            None => (reading.this_year, text),
        };
        let (month, rest) = rest.split_once('月')?;
        let day = rest.trim().trim_end_matches(['日', '号']).trim();
        return date_serial(
            year,
            month.trim().parse().ok()?,
            day.parse().ok()?,
            reading.date1904,
        );
    }
    let separator = ['/', '-', '.']
        .into_iter()
        .find(|sep| text.contains(*sep))?;
    let parts: Vec<&str> = text.split(separator).map(str::trim).collect();
    if parts
        .iter()
        .any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    let numbers: Vec<i64> = parts
        .iter()
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    let year = |y: i64, digits: usize| match digits {
        // Two-digit years: 00–29 are 2000s, 30–99 are 1900s, as in Excel.
        1 | 2 if y < 30 => 2000 + y,
        1 | 2 => 1900 + y,
        _ => y,
    };
    let (y, m, d) = match numbers.as_slice() {
        // A four-digit first number is always a year.
        [a, b, c] if parts[0].len() == 4 => (*a, *b, *c),
        [a, b, c] => match reading.order {
            DateOrder::Ymd => (year(*a, parts[0].len()), *b, *c),
            DateOrder::Mdy => (year(*c, parts[2].len()), *a, *b),
            DateOrder::Dmy => (year(*c, parts[2].len()), *b, *a),
        },
        [a, b] if parts[0].len() == 4 => (*a, *b, 1),
        [a, b] => match reading.order {
            DateOrder::Dmy => (reading.this_year, *b, *a),
            _ => (reading.this_year, *a, *b),
        },
        _ => return None,
    };
    date_serial(
        y,
        u32::try_from(m).ok()?,
        u32::try_from(d).ok()?,
        reading.date1904,
    )
}

/// A time of day as a fraction of a day. Hours may go past 24, as Excel
/// allows for durations.
fn time_of(text: &str) -> Option<f64> {
    let text = text.trim();
    let upper = text.to_ascii_uppercase();
    let (clock, meridiem) = if let Some(rest) = upper.strip_suffix("PM").or(upper.strip_suffix('P'))
    {
        (rest.trim().to_owned(), Some(true))
    } else if let Some(rest) = upper.strip_suffix("AM").or(upper.strip_suffix('A')) {
        (rest.trim().to_owned(), Some(false))
    } else {
        (upper.clone(), None)
    };
    let parts: Vec<&str> = clock.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let mut hours: f64 = parts[0].trim().parse::<u32>().ok()?.into();
    let minutes: f64 = parts[1]
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|m| *m < 60)?
        .into();
    let seconds: f64 = match parts.get(2) {
        Some(s) => s
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|s| (0.0..60.0).contains(s))?,
        None => 0.0,
    };
    if let Some(pm) = meridiem {
        if !(1.0..=12.0).contains(&hours) {
            return None;
        }
        hours %= 12.0;
        if pm {
            hours += 12.0;
        }
    }
    Some((hours * 3600.0 + minutes * 60.0 + seconds) / 86_400.0)
}

/// Function names and cell references in capitals, as Excel stores them;
/// text in quotes and sheet names in apostrophes are left alone.
pub fn normalize_formula(formula: &str) -> String {
    let mut out = String::with_capacity(formula.len());
    let chars: Vec<char> = formula.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' || c == '\'' {
            // Copy the quoted run, doubled quotes included.
            out.push(c);
            i += 1;
            while i < chars.len() {
                out.push(chars[i]);
                if chars[i] == c {
                    if chars.get(i + 1) == Some(&c) {
                        out.push(c);
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        if c.is_alphabetic() || c == '_' || c == '$' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || matches!(chars[i], '_' | '.' | '$'))
            {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let is_function = chars.get(i) == Some(&'(');
            let is_sheet = chars.get(i) == Some(&'!');
            if (is_function || is_reference(&word)) && !is_sheet {
                out.push_str(&word.to_uppercase());
            } else {
                out.push_str(&word);
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// `A1`, `$b$2`, `xfd1048576`: letters then digits, optionally anchored.
fn is_reference(word: &str) -> bool {
    let word = word.trim_start_matches('$');
    let letters = word.chars().take_while(|c| c.is_ascii_alphabetic()).count();
    if letters == 0 || letters > 3 {
        return false;
    }
    let rest = word[letters..].trim_start_matches('$');
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(order: DateOrder) -> Reading {
        Reading {
            order,
            date1904: false,
            this_year: 2026,
        }
    }

    fn value(text: &str) -> Entry {
        read(text, &reading(DateOrder::Ymd)).0
    }

    fn num(v: f64) -> Entry {
        Entry::Value(Value::Number(v))
    }

    fn text(t: &str) -> Entry {
        Entry::Value(Value::Text(Arc::from(t)))
    }

    #[test]
    fn numbers_in_their_usual_spellings() {
        assert_eq!(value("1234.5"), num(1234.5));
        assert_eq!(value(" 42 "), num(42.0));
        assert_eq!(value("-1,234"), num(-1234.0));
        assert_eq!(
            read("1,234,567.25", &reading(DateOrder::Ymd)),
            (num(1_234_567.25), Look::Grouped)
        );
        assert_eq!(
            read("12%", &reading(DateOrder::Ymd)),
            (num(0.12), Look::Percent)
        );
        assert_eq!(
            read("¥1,200", &reading(DateOrder::Ymd)),
            (
                num(1200.0),
                Look::Currency {
                    symbol: '¥',
                    before: true
                }
            )
        );
        assert_eq!(value("$-5"), num(-5.0));
        assert_eq!(value("(12)"), num(-12.0));
        assert_eq!(value("1.5E3"), num(1500.0));
        assert_eq!(value(".5"), num(0.5));
        // Not numbers.
        assert_eq!(value("1,23"), text("1,23"));
        assert_eq!(value("12abc"), text("12abc"));
        assert_eq!(value("一二三"), text("一二三"));
        assert_eq!(value("Infinity"), text("Infinity"));
    }

    #[test]
    fn logical_values_text_and_clearing() {
        assert_eq!(value("true"), Entry::Value(Value::Bool(true)));
        assert_eq!(value("FALSE"), Entry::Value(Value::Bool(false)));
        assert_eq!(value("'123"), text("123"));
        assert_eq!(value("'=A1"), text("=A1"));
        assert_eq!(value(""), Entry::Empty);
        assert_eq!(value("="), text("="));
        assert_eq!(value("東京 タワー"), text("東京 タワー"));
    }

    #[test]
    fn dates_and_times_follow_the_order() {
        let ymd = reading(DateOrder::Ymd);
        let mdy = reading(DateOrder::Mdy);
        let dmy = reading(DateOrder::Dmy);
        assert_eq!(read("2026/11/24", &ymd), (num(46350.0), Look::Date));
        assert_eq!(read("2026-11-24", &dmy).0, num(46350.0));
        assert_eq!(read("11/24/2026", &mdy).0, num(46350.0));
        assert_eq!(read("24/11/2026", &dmy).0, num(46350.0));
        assert_eq!(read("24.11.2026", &dmy).0, num(46350.0));
        assert_eq!(read("26/11/24", &ymd).0, num(46350.0));
        assert_eq!(read("2026年11月24日", &mdy).0, num(46350.0));
        assert_eq!(read("11月24日", &ymd).0, num(46350.0));
        assert_eq!(read("11/24", &mdy).0, num(46350.0));
        assert_eq!(read("24/11", &dmy).0, num(46350.0));
        // A day that does not exist is text.
        assert_eq!(read("2026/2/30", &ymd).0, text("2026/2/30"));
        assert_eq!(read("9:30", &ymd), (num(9.5 / 24.0), Look::Time));
        assert_eq!(
            read("21:30:15", &ymd).0,
            num((21.0 * 3600.0 + 30.0 * 60.0 + 15.0) / 86_400.0)
        );
        assert_eq!(read("9:30 PM", &mdy).0, num(21.5 / 24.0));
        assert_eq!(read("12:00 AM", &mdy).0, num(0.0));
        assert_eq!(
            read("2026/11/24 12:00", &ymd),
            (num(46350.5), Look::DateTime)
        );
        assert_eq!(read("11/24/2026 6:00 PM", &mdy).0, num(46350.75));
        assert_eq!(read("25:61", &ymd).0, text("25:61"));
    }

    #[test]
    fn formulas_are_normalized() {
        assert_eq!(value("=sum(a1:b2)"), Entry::Formula("SUM(A1:B2)".into()));
        assert_eq!(
            value("= vlookup($a$1,'売上 表'!a:b,2,false) "),
            Entry::Formula("VLOOKUP($A$1,'売上 表'!a:b,2,false)".into())
        );
        assert_eq!(
            normalize_formula(r#"concat("sum(a1)",a1,"""x")"#),
            r#"CONCAT("sum(a1)",A1,"""x")"#
        );
        assert_eq!(normalize_formula("Sheet2!b3*2"), "Sheet2!B3*2");
        assert_eq!(
            normalize_formula("_xlfn.xlookup(x1,a:a,b:b)"),
            "_XLFN.XLOOKUP(X1,a:a,b:b)"
        );
        assert_eq!(normalize_formula("名前+1"), "名前+1");
    }
}
