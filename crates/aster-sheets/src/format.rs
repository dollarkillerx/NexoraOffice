//! Number formats: what a value looks like in its cell.
//!
//! The format codes are Excel's (`#,##0.00`, `0%`, `yyyy/m/d h:mm`,
//! `0.00;[Red]-0.00;"-"`). This covers the common ones: General, digit
//! placeholders with thousands separators and scaling, percentages,
//! scientific notation, dates and times (including elapsed hours), literal
//! text, and up to four sections. Not covered yet: fractions (shown as
//! General), conditions in brackets (ignored), colours (ignored: this version
//! does not draw cell styles), and locale codes (`[$-411]`, ignored; month and
//! day names are English).
//!
//! Rounding follows Excel: the value is first taken to 15 significant
//! digits, then rounded half away from zero, so `1.005` with two decimals is
//! `1.01`.

/// A parsed format code.
#[derive(Clone, Debug, PartialEq)]
pub struct Format {
    code: String,
    sections: Vec<Section>,
}

#[derive(Clone, Debug, PartialEq, Default)]
struct Section {
    tokens: Vec<Token>,
    kind: Kind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum Kind {
    #[default]
    General,
    Number,
    Date,
    Text,
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Literal(String),
    General,
    /// A run of digit placeholders and what is between them, before the
    /// decimal point, after it, and in the exponent.
    Number(NumberSpec),
    Date(DatePart),
    AmPm {
        short: bool,
    },
    /// `@`: the text itself.
    Text,
}

#[derive(Clone, Debug, PartialEq, Default)]
struct NumberSpec {
    /// Placeholders before the point, left to right: `0`, `#` or `?`.
    integer: Vec<u8>,
    fraction: Vec<u8>,
    thousands: bool,
    /// Each trailing comma divides by 1000.
    scale: i32,
    percent: i32,
    exponent: Option<Exponent>,
    has_point: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct Exponent {
    plus: bool,
    digits: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DatePart {
    Year2,
    Year4,
    Month,
    Month2,
    MonthShort,
    MonthLong,
    MonthLetter,
    Day,
    Day2,
    DayShort,
    DayLong,
    Hour,
    Hour2,
    Minute,
    Minute2,
    Second,
    Second2,
    /// Fractions of a second, this many digits.
    SubSecond(u8),
    ElapsedHours(u8),
    ElapsedMinutes(u8),
    ElapsedSeconds(u8),
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

impl Format {
    pub fn general() -> Self {
        Self::parse("General")
    }

    /// The built-in formats Excel does not write into the file. Unknown ids
    /// are General. Ids 14 and 22 follow the system's short date in Excel;
    /// here they are year first, as in Chinese and Japanese.
    pub fn builtin(id: u32) -> Self {
        let code = match id {
            1 => "0",
            2 => "0.00",
            3 => "#,##0",
            4 => "#,##0.00",
            9 => "0%",
            10 => "0.00%",
            11 => "0.00E+00",
            14 => "yyyy/m/d",
            15 => "d-mmm-yy",
            16 => "d-mmm",
            17 => "mmm-yy",
            18 => "h:mm AM/PM",
            19 => "h:mm:ss AM/PM",
            20 => "h:mm",
            21 => "h:mm:ss",
            22 => "yyyy/m/d h:mm",
            27 | 36 | 50 | 57 => "yyyy\"年\"m\"月\"",
            28 | 29 | 52 | 53 | 58 => "m\"月\"d\"日\"",
            30 => "m/d/yy",
            31 | 54 => "yyyy\"年\"m\"月\"d\"日\"",
            32 => "h:mm",
            33 => "h:mm:ss",
            37 => "#,##0 ;(#,##0)",
            38 => "#,##0 ;[Red](#,##0)",
            39 => "#,##0.00;(#,##0.00)",
            40 => "#,##0.00;[Red](#,##0.00)",
            45 => "mm:ss",
            46 => "[h]:mm:ss",
            47 => "mm:ss.0",
            48 => "##0.0E+0",
            49 => "@",
            _ => "General",
        };
        Self::parse(code)
    }

    pub fn parse(code: &str) -> Self {
        if code.trim().is_empty() {
            return Self {
                code: code.to_owned(),
                sections: vec![Section {
                    tokens: vec![Token::General],
                    kind: Kind::General,
                }],
            };
        }
        let sections: Vec<Section> = split_sections(code)
            .into_iter()
            .take(4)
            .map(|section| parse_section(&section))
            .collect();
        Self {
            code: code.to_owned(),
            sections,
        }
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    /// The order a date format shows year, month and day in; `None` for a
    /// format that is not a date or shows fewer than two of them.
    pub fn date_order(&self) -> Option<DateOrder> {
        let section = self.sections.first().filter(|s| s.kind == Kind::Date)?;
        let mut order = Vec::new();
        for token in &section.tokens {
            let Token::Date(part) = token else { continue };
            let which = match part {
                DatePart::Year2 | DatePart::Year4 => 'y',
                DatePart::Month
                | DatePart::Month2
                | DatePart::MonthShort
                | DatePart::MonthLong
                | DatePart::MonthLetter => 'm',
                DatePart::Day | DatePart::Day2 => 'd',
                _ => continue,
            };
            if !order.contains(&which) {
                order.push(which);
            }
        }
        match order.as_slice() {
            ['y', 'm', ..] | ['y', 'd', ..] => Some(DateOrder::Ymd),
            ['m', 'd', ..] | ['m', 'y', ..] => Some(DateOrder::Mdy),
            ['d', 'm', ..] | ['d', 'y', ..] => Some(DateOrder::Dmy),
            _ => None,
        }
    }

    /// Whether numbers in this format are dates or times.
    pub fn is_date(&self) -> bool {
        self.sections.first().is_some_and(|s| s.kind == Kind::Date)
    }

    /// A number as it shows in the cell.
    pub fn number(&self, value: f64, date1904: bool) -> String {
        if !value.is_finite() {
            return "#NUM!".to_owned();
        }
        let count = self
            .sections
            .iter()
            .filter(|s| s.kind != Kind::Text)
            .count();
        let (section, value, signed) = match (count, value) {
            (0, _) => return general(value),
            (1, _) => (&self.sections[0], value, true),
            (2, v) if v < 0.0 => (&self.sections[1], -v, false),
            (_, v) if v < 0.0 && count >= 2 => (&self.sections[1], -v, false),
            (n, v) if n >= 3 && v == 0.0 => (&self.sections[2], v, false),
            _ => (&self.sections[0], value, true),
        };
        render_number(section, value, signed, date1904)
    }

    /// Text as it shows in the cell: through the fourth section, or the
    /// first if it is a text-only one.
    pub fn text(&self, text: &str) -> String {
        let section = match self.sections.len() {
            4 => Some(&self.sections[3]),
            1 if self.sections[0].kind == Kind::Text => Some(&self.sections[0]),
            _ => None,
        };
        let Some(section) = section else {
            return text.to_owned();
        };
        let mut out = String::new();
        for token in &section.tokens {
            match token {
                Token::Literal(literal) => out.push_str(literal),
                Token::Text | Token::General => out.push_str(text),
                _ => {}
            }
        }
        out
    }
}

/// Split at `;` outside quotes and brackets.
fn split_sections(code: &str) -> Vec<String> {
    let mut sections = vec![String::new()];
    let mut chars = code.chars();
    while let Some(c) = chars.next() {
        let current = sections.last_mut().expect("never empty");
        match c {
            ';' => sections.push(String::new()),
            '"' => {
                current.push(c);
                for c in chars.by_ref() {
                    current.push(c);
                    if c == '"' {
                        break;
                    }
                }
            }
            '\\' => {
                current.push(c);
                if let Some(c) = chars.next() {
                    current.push(c);
                }
            }
            _ => current.push(c),
        }
    }
    sections
}

fn parse_section(code: &str) -> Section {
    let chars: Vec<char> = code.chars().collect();
    let mut tokens = Vec::new();
    let mut literal = String::new();
    let mut number: Option<NumberSpec> = None;
    // Where in a number we are: integer part, fraction, exponent.
    let mut in_fraction = false;
    let mut in_exponent = false;
    let mut kind = Kind::General;
    let mut i = 0;

    let flush_literal = |literal: &mut String, tokens: &mut Vec<Token>| {
        if !literal.is_empty() {
            tokens.push(Token::Literal(std::mem::take(literal)));
        }
    };

    while i < chars.len() {
        let c = chars[i];
        let lower = c.to_ascii_lowercase();
        match c {
            '"' => {
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    literal.push(chars[i]);
                    i += 1;
                }
                i += 1;
                continue;
            }
            '\\' => {
                if let Some(next) = chars.get(i + 1) {
                    literal.push(*next);
                }
                i += 2;
                continue;
            }
            '_' => {
                // Space as wide as the next character.
                literal.push(' ');
                i += 2;
                continue;
            }
            '*' => {
                // Repeat to fill the cell: not done, the character is dropped.
                i += 2;
                continue;
            }
            '[' => {
                let end = chars[i..]
                    .iter()
                    .position(|&c| c == ']')
                    .map_or(chars.len(), |at| i + at);
                let inner: String = chars[i + 1..end.min(chars.len())].iter().collect();
                let lower_inner = inner.to_ascii_lowercase();
                let elapsed = |letter: char| {
                    !lower_inner.is_empty() && lower_inner.chars().all(|c| c == letter)
                };
                let width = inner.chars().count().min(9) as u8;
                if elapsed('h') {
                    flush_literal(&mut literal, &mut tokens);
                    tokens.push(Token::Date(DatePart::ElapsedHours(width)));
                    kind = Kind::Date;
                } else if elapsed('m') {
                    flush_literal(&mut literal, &mut tokens);
                    tokens.push(Token::Date(DatePart::ElapsedMinutes(width)));
                    kind = Kind::Date;
                } else if elapsed('s') {
                    flush_literal(&mut literal, &mut tokens);
                    tokens.push(Token::Date(DatePart::ElapsedSeconds(width)));
                    kind = Kind::Date;
                } else if let Some(currency) = inner.strip_prefix('$') {
                    // `[$€-407]`: the symbol before the dash is shown.
                    literal.push_str(currency.split('-').next().unwrap_or(""));
                }
                // Colours, conditions and locales are ignored.
                i = end + 1;
                continue;
            }
            '0' | '#' | '?' => {
                if kind != Kind::Date {
                    flush_literal(&mut literal, &mut tokens);
                    let spec = number.get_or_insert_with(NumberSpec::default);
                    let digit = c as u8;
                    if in_exponent {
                        if let Some(exponent) = spec.exponent.as_mut() {
                            exponent.digits.push(digit);
                        }
                    } else if in_fraction {
                        spec.fraction.push(digit);
                    } else {
                        spec.integer.push(digit);
                    }
                    kind = Kind::Number;
                } else if c == '0' {
                    // `ss.000`
                    let mut digits = 0u8;
                    while chars.get(i) == Some(&'0') {
                        digits = digits.saturating_add(1);
                        i += 1;
                    }
                    if let Some(Token::Literal(text)) = tokens.last()
                        && text == "."
                    {
                        tokens.pop();
                    }
                    if literal == "." {
                        literal.clear();
                    }
                    flush_literal(&mut literal, &mut tokens);
                    tokens.push(Token::Date(DatePart::SubSecond(digits.min(3))));
                    continue;
                } else {
                    literal.push(c);
                }
            }
            '.' if number.is_some() && !in_fraction && !in_exponent && kind != Kind::Date => {
                in_fraction = true;
                number.as_mut().expect("checked").has_point = true;
            }
            '.' if number.is_none()
                && kind != Kind::Date
                && matches!(chars.get(i + 1), Some('0' | '#' | '?')) =>
            {
                flush_literal(&mut literal, &mut tokens);
                number = Some(NumberSpec {
                    has_point: true,
                    ..NumberSpec::default()
                });
                in_fraction = true;
                kind = Kind::Number;
            }
            ',' if number.is_some() && !in_exponent => {
                let next_is_digit = matches!(chars.get(i + 1), Some('0' | '#' | '?'));
                let spec = number.as_mut().expect("checked");
                if next_is_digit && !in_fraction {
                    spec.thousands = true;
                } else {
                    spec.scale += 1;
                }
            }
            '%' => {
                if let Some(spec) = number.as_mut() {
                    spec.percent += 1;
                } else {
                    // `0%` handled above; a lone `%` still multiplies.
                    number = Some(NumberSpec {
                        percent: 1,
                        ..NumberSpec::default()
                    });
                }
                literal.push('%');
                kind = Kind::Number;
            }
            'E' | 'e'
                if number.is_some()
                    && !in_exponent
                    && matches!(chars.get(i + 1), Some('+' | '-')) =>
            {
                let plus = chars[i + 1] == '+';
                number.as_mut().expect("checked").exponent = Some(Exponent {
                    plus,
                    digits: Vec::new(),
                });
                in_exponent = true;
                i += 2;
                continue;
            }
            '@' => {
                flush_literal(&mut literal, &mut tokens);
                end_number(&mut number, &mut tokens);
                tokens.push(Token::Text);
                if kind == Kind::General {
                    kind = Kind::Text;
                }
            }
            _ if lower == 'g' && matches_word(&chars, i, "general") => {
                flush_literal(&mut literal, &mut tokens);
                end_number(&mut number, &mut tokens);
                tokens.push(Token::General);
                i += 7;
                continue;
            }
            _ if (lower == 'a') && matches_word(&chars, i, "am/pm") => {
                flush_literal(&mut literal, &mut tokens);
                tokens.push(Token::AmPm { short: false });
                kind = Kind::Date;
                i += 5;
                continue;
            }
            _ if (lower == 'a') && matches_word(&chars, i, "a/p") => {
                flush_literal(&mut literal, &mut tokens);
                tokens.push(Token::AmPm { short: true });
                kind = Kind::Date;
                i += 3;
                continue;
            }
            _ if matches!(lower, 'y' | 'm' | 'd' | 'h' | 's' | 'e') && number.is_none() => {
                let run = chars[i..]
                    .iter()
                    .take_while(|ch| ch.to_ascii_lowercase() == lower)
                    .count();
                flush_literal(&mut literal, &mut tokens);
                let part = match (lower, run) {
                    ('y' | 'e', 1 | 2) => DatePart::Year2,
                    ('y' | 'e', _) => DatePart::Year4,
                    ('m', 1) => DatePart::Month,
                    ('m', 2) => DatePart::Month2,
                    ('m', 3) => DatePart::MonthShort,
                    ('m', 5) => DatePart::MonthLetter,
                    ('m', _) => DatePart::MonthLong,
                    ('d', 1) => DatePart::Day,
                    ('d', 2) => DatePart::Day2,
                    ('d', 3) => DatePart::DayShort,
                    ('d', _) => DatePart::DayLong,
                    ('h', 1) => DatePart::Hour,
                    ('h', _) => DatePart::Hour2,
                    ('s', 1) => DatePart::Second,
                    (_, _) => DatePart::Second2,
                };
                tokens.push(Token::Date(part));
                kind = Kind::Date;
                i += run;
                continue;
            }
            _ => {
                if number.is_some() {
                    end_number(&mut number, &mut tokens);
                    in_fraction = false;
                    in_exponent = false;
                }
                literal.push(c);
            }
        }
        i += 1;
    }
    end_number(&mut number, &mut tokens);
    flush_literal(&mut literal, &mut tokens);
    // A `%` was pushed as a literal and also counted; keep the literal after
    // the number it belongs to (the order in `tokens` is the order in the code).
    resolve_minutes(&mut tokens);
    if kind == Kind::General && tokens.iter().any(|t| matches!(t, Token::General)) {
        kind = Kind::General;
    } else if kind == Kind::General && !tokens.is_empty() {
        // Only literals: shows them instead of the number.
        kind = Kind::Number;
    }
    Section { tokens, kind }
}

fn matches_word(chars: &[char], at: usize, word: &str) -> bool {
    let len = word.chars().count();
    chars.len() >= at + len
        && chars[at..at + len]
            .iter()
            .zip(word.chars())
            .all(|(a, b)| a.to_ascii_lowercase() == b)
}

fn end_number(number: &mut Option<NumberSpec>, tokens: &mut Vec<Token>) {
    if let Some(spec) = number.take() {
        // The `%` literal was pushed into the literal buffer before the
        // number ended; it is flushed after this token, which is its place.
        tokens.push(Token::Number(spec));
    }
}

/// `m` and `mm` are minutes right after an hour or right before a second.
fn resolve_minutes(tokens: &mut [Token]) {
    let positions: Vec<usize> = tokens
        .iter()
        .enumerate()
        .filter(|(_, t)| matches!(t, Token::Date(_)))
        .map(|(i, _)| i)
        .collect();
    for (n, &at) in positions.iter().enumerate() {
        let Token::Date(part) = tokens[at] else {
            continue;
        };
        if !matches!(part, DatePart::Month | DatePart::Month2) {
            continue;
        }
        let previous = n.checked_sub(1).and_then(|p| match tokens[positions[p]] {
            Token::Date(part) => Some(part),
            _ => None,
        });
        let next = positions.get(n + 1).and_then(|&p| match tokens[p] {
            Token::Date(part) => Some(part),
            _ => None,
        });
        let after_hour = matches!(
            previous,
            Some(DatePart::Hour | DatePart::Hour2 | DatePart::ElapsedHours(_))
        );
        let before_second = matches!(
            next,
            Some(DatePart::Second | DatePart::Second2 | DatePart::ElapsedSeconds(_))
        );
        if after_hour || before_second {
            tokens[at] = Token::Date(if part == DatePart::Month {
                DatePart::Minute
            } else {
                DatePart::Minute2
            });
        }
    }
}

fn render_number(section: &Section, value: f64, signed: bool, date1904: bool) -> String {
    match section.kind {
        Kind::Date => render_date(section, value, date1904),
        Kind::General | Kind::Text => {
            let mut out = String::new();
            for token in &section.tokens {
                match token {
                    Token::Literal(text) => out.push_str(text),
                    Token::General => {
                        let shown = general(if signed { value } else { value.abs() });
                        out.push_str(&shown);
                    }
                    Token::Text => out.push_str(&general(value)),
                    _ => {}
                }
            }
            // An empty section hides the value: `0;-0;;@` shows no zeros.
            out
        }
        Kind::Number if split_digits(section).is_some() => {
            let groups = split_digits(section).expect("checked");
            let spec = NumberSpec {
                integer: vec![b'#'],
                percent: groups.iter().map(|g| g.percent).sum(),
                scale: groups.iter().map(|g| g.scale).sum(),
                ..NumberSpec::default()
            };
            let mut digits = render_spec(&spec, value.abs());
            let min: usize = groups
                .iter()
                .map(|g| g.integer.iter().filter(|&&d| d == b'0').count())
                .sum();
            while digits.len() < min {
                digits.insert(0, '0');
            }
            // Hand the digits out from the right; the first group takes the
            // rest.
            let mut parts = vec![String::new(); groups.len()];
            let mut end = digits.len();
            for (n, group) in groups.iter().enumerate().rev() {
                let take = if n == 0 {
                    end
                } else {
                    group.integer.len().min(end)
                };
                parts[n] = digits[end - take..end].to_owned();
                end -= take;
            }
            let mut out = String::new();
            if signed && value < 0.0 && digits.bytes().any(|b| b != b'0') {
                out.push('-');
            }
            let mut n = 0;
            for token in &section.tokens {
                match token {
                    Token::Literal(text) => out.push_str(text),
                    Token::Number(_) => {
                        out.push_str(&parts[n]);
                        n += 1;
                    }
                    _ => {}
                }
            }
            out
        }
        Kind::Number => {
            let mut out = String::new();
            let negative = signed && value < 0.0;
            let mut sign_written = !negative;
            for token in &section.tokens {
                match token {
                    Token::Literal(text) => out.push_str(text),
                    Token::General => {
                        out.push_str(&general(value.abs()));
                    }
                    Token::Number(spec) => {
                        let digits = render_spec(spec, value.abs());
                        if !sign_written {
                            // A value that rounds to zero shows no minus.
                            if digits.bytes().any(|b| (b'1'..=b'9').contains(&b)) {
                                out.push('-');
                            }
                            sign_written = true;
                        }
                        out.push_str(&digits);
                    }
                    _ => {}
                }
            }
            if !sign_written && !out.is_empty() {
                out.insert(0, '-');
            }
            out
        }
    }
}

/// The digit groups of a code like `000-0000` (a postcode) or
/// `(000) 000-0000`: several integer-only placeholder groups in one section,
/// which share the digits of one number.
fn split_digits(section: &Section) -> Option<Vec<&NumberSpec>> {
    let groups: Vec<&NumberSpec> = section
        .tokens
        .iter()
        .filter_map(|t| match t {
            Token::Number(spec) => Some(spec),
            _ => None,
        })
        .collect();
    (groups.len() > 1
        && groups
            .iter()
            .all(|g| !g.has_point && g.exponent.is_none() && !g.thousands))
    .then_some(groups)
}

fn render_spec(spec: &NumberSpec, value: f64) -> String {
    let mut value = value * 100f64.powi(spec.percent) / 1000f64.powi(spec.scale);
    if let Some(exponent) = &spec.exponent {
        // Mantissa with as many integer digits as placeholders (engineering
        // notation for `##0.0E+0`).
        let int_places = spec.integer.len().max(1) as i32;
        let mut exp = 0i32;
        if value != 0.0 {
            exp = value.abs().log10().floor() as i32;
            if int_places > 1 {
                // Exponent a multiple of the integer places.
                exp = exp.div_euclid(int_places) * int_places;
            }
            value /= 10f64.powi(exp);
        }
        let (mut int, frac) = decimal(value, spec.fraction.len());
        // Rounding can carry to 10.0.
        if int.len() as i32 > int_places && int_places == 1 {
            exp += 1;
            let (i2, f2) = decimal(value / 10.0, spec.fraction.len());
            int = i2;
            return compose(spec, &int, &f2) + &exponent_text(exponent, exp);
        }
        return compose(spec, &int, &frac) + &exponent_text(exponent, exp);
    }
    let (int, frac) = decimal(value, spec.fraction.len());
    compose(spec, &int, &frac)
}

fn exponent_text(exponent: &Exponent, exp: i32) -> String {
    let mut out = String::from("E");
    if exp < 0 {
        out.push('-');
    } else if exponent.plus {
        out.push('+');
    }
    let digits = exp.unsigned_abs().to_string();
    let min = exponent.digits.iter().filter(|&&d| d == b'0').count();
    for _ in digits.len()..min {
        out.push('0');
    }
    out.push_str(&digits);
    out
}

/// Fill the placeholders with the digits of the integer and fraction parts.
fn compose(spec: &NumberSpec, int: &str, frac: &str) -> String {
    let mut out = String::new();
    // Integer: at least as many digits as `0` placeholders.
    let min_int = spec.integer.iter().filter(|&&d| d == b'0').count();
    let pad_int = spec.integer.iter().filter(|&&d| d == b'?').count();
    let mut digits = if int == "0" {
        String::new()
    } else {
        int.to_owned()
    };
    while digits.len() < min_int {
        digits.insert(0, '0');
    }
    if spec.thousands {
        digits = group(&digits);
    }
    for _ in digits.len()..min_int + pad_int {
        out.push(' ');
    }
    out.push_str(&digits);
    if spec.has_point {
        // Trailing `#` drop zeros, `?` become spaces, `0` stay.
        let mut fraction: Vec<char> = frac.chars().collect();
        let mut end = fraction.len();
        while end > 0 && fraction[end - 1] == '0' && spec.fraction.get(end - 1) != Some(&b'0') {
            if spec.fraction.get(end - 1) == Some(&b'?') {
                fraction[end - 1] = ' ';
            } else {
                fraction.pop();
            }
            end -= 1;
        }
        out.push('.');
        out.extend(fraction);
    }
    out
}

fn group(digits: &str) -> String {
    let bytes = digits.as_bytes();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// `value` (not negative) rounded to `places` decimals the way Excel does:
/// 15 significant digits first, then half away from zero.
fn decimal(value: f64, places: usize) -> (String, String) {
    let (digits, exp) = significant(value);
    // The digit for 10^p is digits[exp - p].
    let digit = |p: i32| -> u8 {
        let index = exp - p;
        if index >= 0 {
            digits.get(index as usize).copied().unwrap_or(0)
        } else {
            0
        }
    };
    let top = exp.max(0);
    let guard_power = -(places as i32) - 1;
    let mut number: Vec<u8> = (guard_power + 1..=top).rev().map(digit).collect();
    if digit(guard_power) >= 5 {
        let mut k = number.len();
        loop {
            if k == 0 {
                number.insert(0, 1);
                break;
            }
            k -= 1;
            if number[k] == 9 {
                number[k] = 0;
            } else {
                number[k] += 1;
                break;
            }
        }
    }
    let split = number.len() - places;
    let int: String = number[..split].iter().map(|d| (b'0' + d) as char).collect();
    let frac: String = number[split..].iter().map(|d| (b'0' + d) as char).collect();
    let int = int.trim_start_matches('0');
    (
        if int.is_empty() {
            "0".to_owned()
        } else {
            int.to_owned()
        },
        frac,
    )
}

/// The 15 significant digits of `value` (not negative) and the power of ten
/// of the first one.
fn significant(value: f64) -> (Vec<u8>, i32) {
    if value == 0.0 {
        return (vec![0], 0);
    }
    let text = format!("{:.14e}", value);
    let (mantissa, exp) = text.split_once('e').expect("scientific");
    let digits = mantissa
        .bytes()
        .filter(u8::is_ascii_digit)
        .map(|b| b - b'0')
        .collect();
    (digits, exp.parse().expect("exponent"))
}

/// Excel's General format, for a column wide enough for 11 characters.
pub fn general(value: f64) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }
    let negative = value < 0.0;
    let abs = value.abs();
    let (digits, exp) = significant(abs);
    // The value exactly, to 15 significant digits, if that fits.
    let sig = digits.iter().rposition(|&d| d != 0).map_or(1, |i| i + 1) as i32;
    let exact_places = (sig - 1 - exp).max(0) as usize;
    let (int, frac) = decimal(abs, exact_places);
    let exact = if frac.is_empty() {
        int.clone()
    } else {
        format!("{int}.{frac}")
    };
    let body = if exact.len() <= 11 {
        exact
    } else if (-4..11).contains(&exp) {
        // Round to what fits in 11 characters.
        let places = if exp >= 0 {
            (9 - exp).max(0) as usize
        } else {
            9
        };
        let (int, frac) = decimal(abs, places);
        let frac = frac.trim_end_matches('0');
        if int.len() > 11 {
            scientific(abs)
        } else if frac.is_empty() {
            int
        } else {
            format!("{int}.{frac}")
        }
    } else {
        scientific(abs)
    };
    if negative && body != "0" {
        format!("-{body}")
    } else {
        body
    }
}

fn scientific(abs: f64) -> String {
    let mut exp = abs.log10().floor() as i32;
    let mut mantissa = abs / 10f64.powi(exp);
    let (mut int, mut frac) = decimal(mantissa, 5);
    if int.len() > 1 {
        exp += 1;
        mantissa /= 10.0;
        (int, frac) = decimal(mantissa, 5);
    }
    let frac = frac.trim_end_matches('0');
    let sign = if exp < 0 { '-' } else { '+' };
    let mantissa = if frac.is_empty() {
        int
    } else {
        format!("{int}.{frac}")
    };
    format!("{mantissa}E{sign}{:02}", exp.unsigned_abs())
}

/// Year, month (1–12), day, hour, minute, second and milliseconds of a date
/// serial number. In the 1900 system serial 60 is the 29 February 1900 that
/// never was, as in Excel. `None` for negative serials and years past 9999.
pub fn date_parts(serial: f64, date1904: bool) -> Option<[u32; 7]> {
    // 2,958,466 is 10000-01-01 in the 1900 system.
    if !serial.is_finite() || !(0.0..2_958_466.0).contains(&serial) {
        return None;
    }
    let mut days = serial.floor() as i64;
    let mut ms = ((serial - serial.floor()) * 86_400_000.0).round() as i64;
    if ms >= 86_400_000 {
        days += 1;
        ms -= 86_400_000;
    }
    let (y, m, d) = if date1904 {
        civil(days + days_from_civil(1904, 1, 1))
    } else if days == 60 {
        (1900, 2, 29)
    } else if days == 0 {
        (1900, 1, 0)
    } else {
        let days = if days > 60 { days - 1 } else { days };
        civil(days + days_from_civil(1899, 12, 31))
    };
    if y > 9999 {
        return None;
    }
    let total_s = ms / 1000;
    Some([
        y as u32,
        m,
        d,
        (total_s / 3600) as u32,
        (total_s / 60 % 60) as u32,
        (total_s % 60) as u32,
        (ms % 1000) as u32,
    ])
}

/// How a date typed as three numbers is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DateOrder {
    /// 2026/11/24: Chinese, Japanese, Korean, ISO.
    Ymd,
    /// 11/24/2026: American English.
    Mdy,
    /// 24/11/2026: most of Europe.
    Dmy,
}

/// The serial number of a date, as Excel counts: in the 1900 system day 1
/// is 1900-01-01 and day 60 the 29 February 1900 that never was. `None` for
/// dates the system cannot hold or that do not exist.
pub fn date_serial(year: i64, month: u32, day: u32, date1904: bool) -> Option<f64> {
    if !(1..=12).contains(&month) || day == 0 || !(1900..=9999).contains(&year) {
        return None;
    }
    let last = match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        // Excel's own leap day.
        2 if year == 1900 && !date1904 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if day > last {
        return None;
    }
    if date1904 {
        let days = days_from_civil(year, month, day) - days_from_civil(1904, 1, 1);
        return (days >= 0).then_some(days as f64);
    }
    if year == 1900 && month == 2 && day == 29 {
        return Some(60.0);
    }
    let days = days_from_civil(year, month, day) - days_from_civil(1899, 12, 31);
    Some(if days >= 60 { days + 1 } else { days } as f64)
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn render_date(section: &Section, value: f64, date1904: bool) -> String {
    // With fractions of a second shown, round to that many digits first.
    let sub_digits = section
        .tokens
        .iter()
        .filter_map(|t| match t {
            Token::Date(DatePart::SubSecond(d)) => Some(i32::from(*d)),
            _ => None,
        })
        .max();
    let rounded = match sub_digits {
        Some(d) => {
            let units = 86_400.0 * 10f64.powi(d);
            (value * units).round() / units
        }
        None => value,
    };
    let Some(parts) = date_parts(rounded, date1904) else {
        // Excel fills the cell with `#`.
        return "########".to_owned();
    };
    let has_sub = section
        .tokens
        .iter()
        .any(|t| matches!(t, Token::Date(DatePart::SubSecond(_))));
    let [y, mo, d, mut h, mut mi, mut s, ms] = parts;
    let mut ms = ms;
    if !has_sub && ms >= 500 {
        // Without fractions Excel rounds to the nearest second.
        s += 1;
        ms = 0;
        if s == 60 {
            s = 0;
            mi += 1;
        }
        if mi == 60 {
            mi = 0;
            h += 1;
        }
        if h == 24 {
            h = 0;
        }
    }
    let twelve = section
        .tokens
        .iter()
        .any(|t| matches!(t, Token::AmPm { .. }));
    let weekday = {
        let serial_days = value.floor() as i64;
        // Serial 1 (1900-01-01) was a Sunday in Excel's reckoning.
        let base = if date1904 { 6 } else { 0 };
        ((serial_days + base + 6).rem_euclid(7)) as usize
    };
    let total_seconds = value * 86_400.0;
    let mut out = String::new();
    for token in &section.tokens {
        match token {
            Token::Literal(text) => out.push_str(text),
            Token::AmPm { short } => {
                let pm = h >= 12;
                out.push_str(match (short, pm) {
                    (false, false) => "AM",
                    (false, true) => "PM",
                    (true, false) => "A",
                    (true, true) => "P",
                });
            }
            Token::Date(part) => {
                let hour12 = if twelve {
                    match h % 12 {
                        0 => 12,
                        other => other,
                    }
                } else {
                    h
                };
                let text = match part {
                    DatePart::Year2 => format!("{:02}", y % 100),
                    DatePart::Year4 => format!("{y:04}"),
                    DatePart::Month => mo.to_string(),
                    DatePart::Month2 => format!("{mo:02}"),
                    DatePart::MonthShort => MONTHS[(mo.max(1) - 1) as usize][..3].to_owned(),
                    DatePart::MonthLong => MONTHS[(mo.max(1) - 1) as usize].to_owned(),
                    DatePart::MonthLetter => MONTHS[(mo.max(1) - 1) as usize][..1].to_owned(),
                    DatePart::Day => d.to_string(),
                    DatePart::Day2 => format!("{d:02}"),
                    DatePart::DayShort => DAYS[weekday][..3].to_owned(),
                    DatePart::DayLong => DAYS[weekday].to_owned(),
                    DatePart::Hour => hour12.to_string(),
                    DatePart::Hour2 => format!("{hour12:02}"),
                    DatePart::Minute => mi.to_string(),
                    DatePart::Minute2 => format!("{mi:02}"),
                    DatePart::Second => s.to_string(),
                    DatePart::Second2 => format!("{s:02}"),
                    DatePart::SubSecond(digits) => {
                        let text = format!("{ms:03}");
                        format!(".{}", &text[..usize::from(*digits).clamp(1, 3)])
                    }
                    DatePart::ElapsedHours(width) => {
                        pad((total_seconds / 3600.0).floor() as i64, *width)
                    }
                    DatePart::ElapsedMinutes(width) => {
                        pad((total_seconds / 60.0).floor() as i64, *width)
                    }
                    DatePart::ElapsedSeconds(width) => pad(total_seconds.round() as i64, *width),
                };
                out.push_str(&text);
            }
            Token::General | Token::Number(_) | Token::Text => {}
        }
    }
    out
}

fn pad(value: i64, width: u8) -> String {
    format!("{value:0width$}", width = usize::from(width))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_to_serials_and_back() {
        assert_eq!(date_serial(2026, 11, 24, false), Some(46350.0));
        assert_eq!(date_serial(1900, 1, 1, false), Some(1.0));
        assert_eq!(date_serial(1900, 2, 28, false), Some(59.0));
        assert_eq!(date_serial(1900, 2, 29, false), Some(60.0));
        assert_eq!(date_serial(1900, 3, 1, false), Some(61.0));
        assert_eq!(date_serial(1904, 1, 1, true), Some(0.0));
        assert_eq!(date_serial(2026, 2, 29, false), None);
        assert_eq!(date_serial(2024, 2, 29, false), Some(45351.0));
        assert_eq!(date_serial(2026, 13, 1, false), None);
        for serial in [1.0, 59.0, 61.0, 45351.0, 46350.0] {
            let [y, m, d, ..] = date_parts(serial, false).unwrap();
            assert_eq!(date_serial(i64::from(y), m, d, false), Some(serial));
        }
        assert_eq!(Format::parse("yyyy/m/d").date_order(), Some(DateOrder::Ymd));
        assert_eq!(Format::parse("m/d/yy").date_order(), Some(DateOrder::Mdy));
        assert_eq!(
            Format::parse("dd.mm.yyyy").date_order(),
            Some(DateOrder::Dmy)
        );
        assert_eq!(Format::parse("h:mm").date_order(), None);
        assert_eq!(Format::parse("0.00").date_order(), None);
    }

    fn show(code: &str, value: f64) -> String {
        Format::parse(code).number(value, false)
    }

    #[test]
    fn general_numbers() {
        for (value, expected) in [
            (0.0, "0"),
            (1.0, "1"),
            (-1.5, "-1.5"),
            (0.1 + 0.2, "0.3"),
            (1.0 / 3.0, "0.333333333"),
            (123_456_789.0, "123456789"),
            (12_345_678_901.0, "12345678901"),
            (123_456_789_012.0, "1.23457E+11"),
            (0.000_000_001_234, "1.234E-09"),
            (2.0 / 3.0 * 1000.0, "666.6666667"),
            (1e-300, "1E-300"),
        ] {
            assert_eq!(general(value), expected, "{value}");
        }
    }

    #[test]
    fn digit_placeholders_thousands_and_rounding() {
        assert_eq!(show("0", 2.5), "3");
        assert_eq!(show("0", -2.5), "-3");
        assert_eq!(show("0.00", 1.005), "1.01");
        assert_eq!(show("0.00", 0.0), "0.00");
        assert_eq!(show("#,##0", 1_234_567.4), "1,234,567");
        assert_eq!(show("#,##0.00", -1234.5), "-1,234.50");
        assert_eq!(show("#,##0.00", 999.999), "1,000.00");
        assert_eq!(show("#.##", 0.5), ".5");
        assert_eq!(show("0.0#", 1.0), "1.0");
        assert_eq!(show("000", 7.0), "007");
        assert_eq!(show("#,##0,", 1_234_567.0), "1,235");
        assert_eq!(show("0.00", -0.001), "0.00");
        assert_eq!(show("\"¥\"#,##0", 3000.0), "¥3,000");
        assert_eq!(show("[$€-407] #,##0.00", 12.0), "€ 12.00");
        assert_eq!(show("#,##0\"円\"", 1980.0), "1,980円");
        assert_eq!(show("0.0\" 万元\"", 12.34), "12.3 万元");
        assert_eq!(show("000-0000", 1_000_001.0), "100-0001");
        assert_eq!(show("000-0000", 1234.0), "000-1234");
        assert_eq!(show("(000) 000-0000", 3_125_551_234.0), "(312) 555-1234");
        assert_eq!(show("#,##0_);(#,##0)", -5.0), "(5)");
        assert_eq!(show("#,##0_);(#,##0)", 5.0), "5 ");
        assert_eq!(show("0;-0;;@", 0.0), "");
        assert_eq!(Format::parse("").number(1.5, false), "1.5");
    }

    #[test]
    fn sections_pick_by_sign() {
        assert_eq!(show("0.00;(0.00)", -3.0), "(3.00)");
        assert_eq!(show("0;-0;\"-\"", 0.0), "-");
        assert_eq!(show("0;-0;\"-\"", 5.0), "5");
        assert_eq!(show("#,##0 ;[Red](#,##0)", -1200.0), "(1,200)");
        assert_eq!(show("#,##0 ;[Red](#,##0)", 1200.0), "1,200 ");
        let f = Format::parse("0;0;0;\"文字: \"@");
        assert_eq!(f.text("东京"), "文字: 东京");
        assert_eq!(Format::parse("0.00").text("abc"), "abc");
        assert_eq!(Format::parse("@").text("abc"), "abc");
    }

    #[test]
    fn percentages_and_scientific() {
        assert_eq!(show("0%", 0.256), "26%");
        assert_eq!(show("0.00%", -0.0123), "-1.23%");
        assert_eq!(show("0.00E+00", 12345.0), "1.23E+04");
        assert_eq!(show("0.00E+00", 0.000_123), "1.23E-04");
        assert_eq!(show("0.00E+00", 9.999), "1.00E+01");
        assert_eq!(show("##0.0E+0", 12345.0), "12.3E+3");
    }

    #[test]
    fn dates_and_times() {
        assert_eq!(date_parts(1.0, false), Some([1900, 1, 1, 0, 0, 0, 0]));
        assert_eq!(date_parts(59.0, false), Some([1900, 2, 28, 0, 0, 0, 0]));
        assert_eq!(date_parts(60.0, false), Some([1900, 2, 29, 0, 0, 0, 0]));
        assert_eq!(date_parts(61.0, false), Some([1900, 3, 1, 0, 0, 0, 0]));
        assert_eq!(
            date_parts(46_348.5, false),
            Some([2026, 11, 22, 12, 0, 0, 0])
        );
        assert_eq!(date_parts(0.0, true), Some([1904, 1, 1, 0, 0, 0, 0]));
        assert_eq!(date_parts(-1.0, false), None);

        let date = 46_348.0 + (14.0 * 3600.0 + 5.0 * 60.0 + 9.0) / 86_400.0;
        assert_eq!(show("yyyy/m/d", date), "2026/11/22");
        assert_eq!(show("yyyy-mm-dd hh:mm:ss", date), "2026-11-22 14:05:09");
        assert_eq!(show("h:mm AM/PM", date), "2:05 PM");
        assert_eq!(show("d-mmm-yy", date), "22-Nov-26");
        assert_eq!(show("dddd, mmmm d", date), "Sunday, November 22");
        assert_eq!(show("yyyy\"年\"m\"月\"d\"日\"", date), "2026年11月22日");
        assert_eq!(show("[h]:mm:ss", 1.5), "36:00:00");
        assert_eq!(show("mm:ss.0", 1.0 / 86_400.0 * 61.25), "01:01.3");
        assert_eq!(show("h:mm", 0.999_999_9), "0:00");
        assert!(Format::builtin(14).is_date());
        assert!(!Format::builtin(4).is_date());
        assert_eq!(Format::builtin(22).number(date, false), "2026/11/22 14:05");
        assert_eq!(show("yyyy/m/d", 3_000_000.0), "########");
    }

    #[test]
    fn literal_only_and_odd_codes_do_not_panic() {
        assert_eq!(show("\"n/a\"", 5.0), "n/a");
        for code in [
            "", ";", ";;;", "[", "\"", "\\", "_", "*", "0.", ".0", "E+", "0E+", "#,", "%", "[Red]",
            "[>100]0", "# ?/?", "????.???", "yyyy\\", "[hh", "0;", "@@", "0,,,",
        ] {
            let format = Format::parse(code);
            for value in [0.0, -1.0, 1.5, 1e20, 1e-20, 123_456.789] {
                let _ = format.number(value, false);
            }
            let _ = format.text("テキスト");
        }
    }
}
