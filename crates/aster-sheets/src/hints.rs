//! Hints while a formula is typed: the functions and defined names that
//! complete the word at the caret, and the signature of the function the
//! caret is inside, with the argument it is on.
//!
//! This reads text only; it asks no engine. The list of functions is
//! `functions.txt`, which a test in `aster-calc` holds against what the
//! engine registers. Nothing here runs per cell or per frame: it runs once
//! per key, over the text before the caret, which a formula keeps short (a
//! formula longer than [`MAX_SCAN`] gets no hints).

use std::sync::OnceLock;

/// A formula longer than this (in bytes before the caret) gets no hints, so
/// that typing into a huge text is never slowed down.
pub const MAX_SCAN: usize = 8192;

/// The completions offered at most.
pub const MAX_ITEMS: usize = 200;

/// One argument of a function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arg {
    pub name: String,
    pub optional: bool,
}

/// A function the engine implements.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Function {
    /// In capitals, as it is typed.
    pub name: String,
    pub args: Vec<Arg>,
    /// The last argument, or the last [`Function::group`] of them, may
    /// repeat.
    pub repeats: bool,
    /// How many arguments the repeating end has: 1, or 2 for pairs.
    pub group: usize,
    /// What it does, in English.
    pub description: String,
}

impl Function {
    /// The call as written: `SUM(number1, [number2], …)`.
    pub fn signature(&self) -> String {
        let (before, current, after) = self.split(None);
        format!("{before}{current}{after}")
    }

    /// The signature in three parts around argument `index`, which is the
    /// part to show in bold: the text before it, it, the text after it.
    /// `None`, or an index past the arguments of a function that does not
    /// repeat, gives the whole signature as the first part and nothing to
    /// show in bold.
    pub fn split(&self, index: Option<usize>) -> (String, String, String) {
        let n = self.args.len();
        let shown = |i: usize| {
            let arg = &self.args[i];
            let mut text = if arg.optional {
                format!("[{}]", arg.name)
            } else {
                arg.name.clone()
            };
            if self.repeats && i + 1 == n {
                text.push_str(", …");
            }
            text
        };
        let list = |from: usize, to: usize| (from..to).map(shown).collect::<Vec<_>>().join(", ");
        let index = index
            .map(|i| {
                if i >= n && self.repeats && n > 0 {
                    let group = self.group.clamp(1, n);
                    n - group + (i - n) % group
                } else {
                    i
                }
            })
            .filter(|&i| i < n);
        match index {
            None => (
                format!("{}({})", self.name, list(0, n)),
                String::new(),
                String::new(),
            ),
            Some(i) => {
                let mut before = format!("{}(", self.name);
                if i > 0 {
                    before.push_str(&list(0, i));
                    before.push_str(", ");
                }
                let mut after = String::new();
                if i + 1 < n {
                    after.push_str(", ");
                    after.push_str(&list(i + 1, n));
                }
                after.push(')');
                (before, shown(i), after)
            }
        }
    }
}

/// Every function, sorted by name.
pub struct Catalogue {
    functions: Vec<Function>,
}

impl Catalogue {
    fn parse(text: &str) -> Self {
        let mut functions: Vec<Function> = Vec::new();
        let mut aliases: Vec<(&str, &str)> = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((alias, target)) = line.split_once(" = ") {
                aliases.push((alias.trim(), target.trim()));
                continue;
            }
            let mut parts = line.splitn(3, '|');
            let name = parts.next().unwrap_or_default().trim();
            let (args, description) = match (parts.next(), parts.next()) {
                (Some(args), Some(description)) => (args, description),
                (Some(description), None) => ("", description),
                _ => continue,
            };
            let mut repeats = false;
            let mut group = 1;
            let mut list = Vec::new();
            // `[a, b]...` is a pair that repeats; `[b]...` a single one.
            for piece in split_args(args) {
                let piece = piece.trim();
                let (piece, repeat) = match piece.strip_suffix("...") {
                    Some(rest) => (rest, true),
                    None => (piece, false),
                };
                let (inner, optional) =
                    match piece.strip_prefix('[').and_then(|p| p.strip_suffix(']')) {
                        Some(inner) => (inner, true),
                        None => (piece, false),
                    };
                let members: Vec<&str> = inner
                    .split(',')
                    .map(str::trim)
                    .filter(|m| !m.is_empty())
                    .collect();
                if repeat {
                    repeats = true;
                    group = members.len().max(1);
                }
                for member in members {
                    list.push(Arg {
                        name: member.to_owned(),
                        optional,
                    });
                }
            }
            functions.push(Function {
                name: name.to_owned(),
                args: list,
                repeats,
                group,
                description: description.trim().to_owned(),
            });
        }
        for (alias, target) in aliases {
            if let Some(found) = functions.iter().find(|f| f.name == target) {
                let mut copy = found.clone();
                copy.name = alias.to_owned();
                functions.push(copy);
            }
        }
        functions.sort_by(|a, b| a.name.cmp(&b.name));
        Self { functions }
    }

    pub fn all(&self) -> &[Function] {
        &self.functions
    }

    /// The function called `name`, in any case.
    pub fn get(&self, name: &str) -> Option<&Function> {
        let name = name.to_ascii_uppercase();
        self.functions
            .binary_search_by(|f| f.name.as_str().cmp(name.as_str()))
            .ok()
            .map(|i| &self.functions[i])
    }

    /// The functions whose name starts with `prefix`, in any case.
    pub fn starting_with(&self, prefix: &str) -> &[Function] {
        let prefix = prefix.to_ascii_uppercase();
        let start = self
            .functions
            .partition_point(|f| f.name.as_str() < prefix.as_str());
        let len = self.functions[start..]
            .iter()
            .take_while(|f| f.name.starts_with(&prefix))
            .count();
        &self.functions[start..start + len]
    }
}

/// `a, [b, c]...` split at the commas outside brackets.
fn split_args(args: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut current = String::new();
    for c in args.chars() {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    if !current.trim().is_empty() {
        out.push(current);
    }
    out
}

/// The functions the engine implements.
pub fn catalogue() -> &'static Catalogue {
    static CATALOGUE: OnceLock<Catalogue> = OnceLock::new();
    CATALOGUE.get_or_init(|| Catalogue::parse(include_str!("functions.txt")))
}

/// What a completion is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Function,
    Name,
}

/// One entry of the list of completions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    pub name: String,
    pub kind: Kind,
    /// What the function does, or what the name stands for.
    pub detail: String,
}

/// The signature of the function the caret is inside.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub function: &'static Function,
    /// The argument the caret is on, from 0.
    pub index: usize,
}

/// What a formula being typed offers at the caret.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hints {
    /// Where the word being completed starts, a byte offset; its end is the
    /// caret.
    pub word_start: usize,
    pub items: Vec<Completion>,
    pub call: Option<Call>,
    /// The text after the caret starts with `(`: completing a name there
    /// adds no second one.
    pub paren_follows: bool,
}

#[derive(Debug)]
struct Frame {
    /// The function this parenthesis opens; `None` for a plain group.
    function: Option<String>,
    commas: usize,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '.'
}

/// The hints at byte offset `caret` of `text`. Nothing unless the text is a
/// formula (starts with `=`) and the caret is outside quotes. `names` are
/// the defined names that apply, with what each stands for.
pub fn hints(text: &str, caret: usize, names: &[(&str, &str)]) -> Hints {
    let mut out = Hints::default();
    if !text.starts_with('=') || caret > text.len() || !text.is_char_boundary(caret) {
        return out;
    }
    if caret > MAX_SCAN {
        return out;
    }
    let before = &text[..caret];
    // One pass over the text before the caret: quotes, parentheses, commas.
    let mut frames: Vec<Frame> = Vec::new();
    let mut in_text = false;
    let mut in_sheet = false;
    let mut braces = 0usize;
    let mut word_start: Option<usize> = None;
    for (at, c) in before.char_indices().skip(1) {
        if in_text {
            if c == '"' {
                in_text = false;
            }
            continue;
        }
        if in_sheet {
            if c == '\'' {
                in_sheet = false;
            }
            continue;
        }
        if is_word(c) {
            if word_start.is_none() {
                word_start = Some(at);
            }
            continue;
        }
        let word = word_start.take().map(|start| (start, at));
        match c {
            '"' => in_text = true,
            '\'' => in_sheet = true,
            '{' => braces += 1,
            '}' => braces = braces.saturating_sub(1),
            '(' => {
                let function = word
                    .map(|(start, end)| &before[start..end])
                    .filter(|w| {
                        w.chars()
                            .next()
                            .is_some_and(|c| c.is_alphabetic() || c == '_')
                    })
                    .map(str::to_owned);
                frames.push(Frame {
                    function,
                    commas: 0,
                });
            }
            ')' => {
                frames.pop();
            }
            ',' if braces == 0 => {
                if let Some(frame) = frames.last_mut() {
                    frame.commas += 1;
                }
            }
            _ => {}
        }
    }
    if in_text || in_sheet {
        return out;
    }
    // The signature of the innermost call.
    if let Some(frame) = frames.iter().rev().find(|f| f.function.is_some())
        && let Some(function) = frame
            .function
            .as_deref()
            .and_then(|name| catalogue().get(name))
    {
        out.call = Some(Call {
            function,
            index: frame.commas,
        });
    }
    // The word at the caret: one that starts with a letter, and does not
    // belong to a reference (`Sheet1!A1`, `$A`).
    let Some(start) = word_start else {
        return out;
    };
    let word = &before[start..];
    let starts_like_a_name = word
        .chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_');
    let after_reference = before[..start].ends_with(['!', '$', ':']);
    if !starts_like_a_name || after_reference {
        return out;
    }
    out.word_start = start;
    out.paren_follows = text[caret..].starts_with('(');
    let lower = word.to_lowercase();
    for function in catalogue().starting_with(word) {
        out.items.push(Completion {
            name: function.name.clone(),
            kind: Kind::Function,
            detail: function.description.clone(),
        });
    }
    for (name, stands_for) in names {
        if name.to_lowercase().starts_with(&lower) && !name.starts_with("_xlnm.") {
            out.items.push(Completion {
                name: (*name).to_owned(),
                kind: Kind::Name,
                detail: (*stands_for).to_owned(),
            });
        }
    }
    out.items.sort_by_cached_key(|a| a.name.to_lowercase());
    out.items.truncate(MAX_ITEMS);
    // A name typed in full, with nothing else to choose, has nothing to
    // complete: Enter should enter the formula.
    if out.items.len() == 1
        && out.items[0].kind == Kind::Name
        && out.items[0].name.eq_ignore_ascii_case(word)
    {
        out.items.clear();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at_end(text: &str) -> Hints {
        hints(text, text.len(), &[])
    }

    fn names<'a>(list: &[&'a str]) -> Vec<(&'a str, &'static str)> {
        list.iter().map(|n| (*n, "Sheet1!$A$1")).collect()
    }

    #[test]
    fn the_list_parses_and_is_sorted() {
        let all = catalogue().all();
        assert!(all.len() > 400, "{}", all.len());
        assert!(all.windows(2).all(|w| w[0].name < w[1].name));
        let sum = catalogue().get("sum").unwrap();
        assert_eq!(sum.signature(), "SUM(number1, [number2], …)");
        assert!(sum.repeats && !sum.description.is_empty());
        let pi = catalogue().get("PI").unwrap();
        assert!(pi.args.is_empty());
        assert_eq!(pi.signature(), "PI()");
        // An alias carries its target's arguments.
        assert_eq!(
            catalogue().get("STDEV").unwrap().args,
            catalogue().get("STDEV.S").unwrap().args
        );
        let vlookup = catalogue().get("VLOOKUP").unwrap();
        assert_eq!(
            vlookup.signature(),
            "VLOOKUP(lookup_value, table_array, col_index_num, [range_lookup])"
        );
        assert!(
            all.iter().all(|f| f
                .name
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '.')),
            "names are typed in capitals"
        );
    }

    #[test]
    fn a_prefix_lists_the_functions_that_start_with_it() {
        let found = at_end("=su");
        let list: Vec<&str> = found.items.iter().map(|i| i.name.as_str()).collect();
        assert!(list.contains(&"SUM") && list.contains(&"SUMIF") && list.contains(&"SUBTOTAL"));
        assert!(list.iter().all(|n| n.starts_with("SU")));
        assert_eq!(found.word_start, 1);
        assert!(found.call.is_none());
        // After an operator or a parenthesis too.
        assert_eq!(at_end("=1+ab").word_start, 3);
        assert!(at_end("=SUM(ab").items.iter().any(|i| i.name == "ABS"));
        // Names with dots.
        assert!(
            at_end("=stdev.")
                .items
                .iter()
                .map(|i| i.name.as_str())
                .eq(["STDEV.P", "STDEV.S"])
        );
    }

    #[test]
    fn nothing_is_offered_where_a_word_is_not_a_function() {
        for text in [
            "su",         // not a formula
            "=",          // no word yet
            "=1+",        // no word yet
            "=\"su",      // inside text
            "=A1&\"x su", // inside text
            "='My su",    // inside a sheet name
            "=Sheet1!su", // a reference
            "=$su",       // a reference
            "=3su",       // a number
            "=zzzz",      // nothing starts with it
        ] {
            assert!(at_end(text).items.is_empty(), "{text}");
        }
        // The caret in the middle of a formula looks only at what is before it.
        let found = hints("=su+1", 3, &[]);
        assert!(!found.items.is_empty());
        assert_eq!(found.word_start, 1);
        // A quote closed before the word does not hide it.
        assert!(!at_end("=\"a\"&su").items.is_empty());
    }

    #[test]
    fn defined_names_complete_too() {
        let known = names(&["TaxRate", "Total", "_xlnm.Print_Area", "Other"]);
        let found = hints("=ta", 3, &known);
        let list: Vec<(&str, Kind)> = found
            .items
            .iter()
            .map(|i| (i.name.as_str(), i.kind))
            .collect();
        assert!(list.contains(&("TaxRate", Kind::Name)));
        assert!(list.contains(&("TAN", Kind::Function)));
        assert!(!list.iter().any(|(n, _)| n.starts_with("Other")));
        let found = hints("=_x", 3, &known);
        assert!(found.items.is_empty(), "Excel's own names are not offered");
        // A name typed in full has nothing left to complete.
        let found = hints("=TaxRate", 8, &known);
        assert!(found.items.is_empty());
        // But it is still a list if more start with it.
        let found = hints("=Tot", 4, &names(&["Total", "Totals"]));
        assert_eq!(
            found.items.len(),
            2 + catalogue().starting_with("Tot").len()
        );
    }

    #[test]
    fn the_signature_follows_the_argument_the_caret_is_on() {
        let on = |text: &str| {
            at_end(text)
                .call
                .map(|c| (c.function.name.clone(), c.index))
        };
        assert_eq!(on("=SUM("), Some(("SUM".into(), 0)));
        assert_eq!(on("=SUM(1,"), Some(("SUM".into(), 1)));
        assert_eq!(on("=if(a1>1, 2,"), Some(("IF".into(), 2)));
        // Commas in text and in nested calls and arrays do not count.
        assert_eq!(on("=IF(A1=\"a,b\","), Some(("IF".into(), 1)));
        assert_eq!(on("=IF(SUM(1,2,3),"), Some(("IF".into(), 1)));
        assert_eq!(on("=SUM(1,{1,2,3},"), Some(("SUM".into(), 2)));
        // Inside the inner call, that call's.
        assert_eq!(on("=IF(SUM(1,2"), Some(("SUM".into(), 1)));
        // A plain group of parentheses leaves the call it is in.
        assert_eq!(on("=IF((1+2)*3,"), Some(("IF".into(), 1)));
        assert_eq!(on("=IF((1+2"), Some(("IF".into(), 0)));
        // Closed: no call. Unknown functions: none.
        assert_eq!(on("=SUM(1,2)"), None);
        assert_eq!(on("=NOSUCH(1,"), None);
        assert_eq!(on("=1+2"), None);
        // While a word is typed in an argument, both are offered.
        let found = at_end("=SUM(1,ab");
        assert!(found.call.is_some() && !found.items.is_empty());
        // Hints are for formulas only.
        assert_eq!(on("SUM(1,"), None);
    }

    #[test]
    fn the_signature_is_split_around_the_current_argument() {
        let vlookup = catalogue().get("VLOOKUP").unwrap();
        assert_eq!(
            vlookup.split(Some(0)),
            (
                "VLOOKUP(".into(),
                "lookup_value".into(),
                ", table_array, col_index_num, [range_lookup])".into()
            )
        );
        assert_eq!(
            vlookup.split(Some(2)),
            (
                "VLOOKUP(lookup_value, table_array, ".into(),
                "col_index_num".into(),
                ", [range_lookup])".into()
            )
        );
        assert_eq!(
            vlookup.split(Some(3)),
            (
                "VLOOKUP(lookup_value, table_array, col_index_num, ".into(),
                "[range_lookup]".into(),
                ")".into()
            )
        );
        // Past the last argument of a function that does not repeat.
        let (before, current, after) = vlookup.split(Some(4));
        assert_eq!(format!("{before}{current}{after}"), vlookup.signature());
        assert!(current.is_empty());
        // A function that repeats keeps the last argument in bold.
        let sum = catalogue().get("SUM").unwrap();
        assert_eq!(sum.split(Some(0)).1, "number1");
        assert_eq!(sum.split(Some(1)).1, "[number2], …");
        assert_eq!(sum.split(Some(7)).1, "[number2], …");
        // No arguments at all.
        assert_eq!(
            catalogue().get("NOW").unwrap().split(Some(0)),
            ("NOW()".into(), String::new(), String::new())
        );
    }

    #[test]
    fn long_and_odd_input_costs_nothing() {
        let long = format!("={}su", "1+".repeat(MAX_SCAN));
        assert!(hints(&long, long.len(), &[]).items.is_empty());
        // A caret inside a character, or past the end, finds nothing.
        assert!(hints("=あ", 2, &[]).items.is_empty());
        assert!(hints("=su", 99, &[]).items.is_empty());
        // Non-ASCII words are words; none of them is a function.
        assert!(at_end("=合計").items.is_empty());
    }
}
