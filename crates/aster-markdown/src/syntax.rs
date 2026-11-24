//! What the highlighter and the preview agree on: which Markdown is read,
//! where front matter is, how link labels compare, where bare web addresses
//! are, and what a double click calls a word.

use std::ops::Range;

use icu_segmenter::WordSegmenter;
use icu_segmenter::options::WordBreakInvariantOptions;
use pulldown_cmark::{Event, MetadataBlockKind, Options, Parser, Tag, TagEnd};

/// The Markdown both the editor and the preview read: CommonMark with GitHub's
/// tables, task lists, strikethrough and alerts, plus footnotes and `$` math.
/// Math and footnotes are recognised so they are not mistaken for something
/// else; the preview shows them as code.
pub fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_MATH
        | Options::ENABLE_GFM
}

/// Front matter is only looked for this far into a document, so an opening
/// `---` that is never closed costs little to look at on every key press.
/// Real front matter is a few hundred bytes.
pub const FRONT_MATTER_LIMIT: usize = 64 * 1024;

/// The YAML front matter at the very start of `text`, as Jekyll, Hugo and
/// Obsidian write it: a `---` line, lines of YAML (the first one not blank),
/// and a closing `---` or `...` line. The range runs from 0 to the end of the
/// closing line, its line break included. Only found when the closing line
/// ends within [`FRONT_MATTER_LIMIT`] bytes.
///
/// pulldown-cmark's `ENABLE_YAML_STYLE_METADATA_BLOCKS` has the last word on
/// what counts, but it is not in [`options`]: the parser takes such a block
/// after any blank line, so `---`, a line and `---` in the middle of a
/// document would vanish from the preview. Callers take the front matter off
/// the start and parse the rest on its own.
pub fn front_matter(text: &str) -> Option<Range<usize>> {
    let mut lines = text.split_inclusive('\n');
    let opening = lines.next()?;
    if !opening
        .strip_prefix("---")?
        .bytes()
        .all(|b| b.is_ascii_whitespace())
    {
        return None;
    }
    let mut end = opening.len();
    for (index, line) in lines.enumerate() {
        let start = end;
        end += line.len();
        if end > FRONT_MATTER_LIMIT {
            return None;
        }
        let closing = closes_front_matter(line);
        // The first line can be neither blank nor the end.
        if index == 0 && (closing || line.trim_matches([' ', '\t', '\r', '\n']).is_empty()) {
            return None;
        }
        if closing {
            return parser_agrees(&text[..end], start).then_some(0..end);
        }
    }
    None
}

/// A line that ends front matter: exactly `---` or `...`, then only spaces.
fn closes_front_matter(line: &str) -> bool {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let line = line.strip_suffix('\r').unwrap_or(line);
    matches!(line.trim_end_matches(' '), "---" | "...")
}

/// Whether pulldown-cmark reads `text` as one metadata block from its start
/// to the closing line, which starts at `closing`.
fn parser_agrees(text: &str, closing: usize) -> bool {
    let mut events =
        Parser::new_ext(text, Options::ENABLE_YAML_STYLE_METADATA_BLOCKS).into_offset_iter();
    let yaml = MetadataBlockKind::YamlStyle;
    if !matches!(
        events.next(),
        Some((Event::Start(Tag::MetadataBlock(kind)), range)) if kind == yaml && range.start == 0
    ) {
        return false;
    }
    events.any(|(event, range)| {
        matches!(event, Event::End(TagEnd::MetadataBlock(_))) && range.end >= closing
    })
}

/// The word around byte `offset` of `text`, the way a double click picks one:
/// the segment the character there is in, as ICU4X's word segmenter cuts the
/// text with its Chinese and Japanese dictionary. That is the segmentation
/// Parley gives the editor, so a run of Han characters splits into the same
/// words in both. A run of spaces and a punctuation mark are segments of
/// their own. At the end of the text, the last segment.
pub fn word_range(text: &str, offset: usize) -> Range<usize> {
    if text.is_empty() {
        return 0..0;
    }
    let mut at = offset.min(text.len());
    if at == text.len() {
        at -= 1;
    }
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    let segmenter = WordSegmenter::new_dictionary(WordBreakInvariantOptions::default());
    let mut start = 0;
    for boundary in segmenter.segment_str(text) {
        if boundary > at {
            return start..boundary;
        }
        start = boundary;
    }
    start..text.len()
}

/// A link label the way CommonMark compares them: case folded, with runs of
/// white space collapsed. Lowercasing stands in for full Unicode case folding.
pub fn normalize_label(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    for word in label.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.extend(word.chars().flat_map(char::to_lowercase));
    }
    out
}

/// The label of a link reference definition (`[label]: destination`) on this
/// line, if it starts one. Only the top level is looked at, not definitions
/// inside quotes or list items.
pub fn definition_label(line: &str) -> Option<String> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = line[indent..].strip_prefix('[')?;
    if rest.starts_with('^') {
        return None;
    }
    let mut escaped = false;
    for (index, c) in rest.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '[' => return None,
            ']' => {
                if !rest[index + 1..].starts_with(':') {
                    return None;
                }
                let label = normalize_label(&rest[..index]);
                return (!label.is_empty()).then_some(label);
            }
            _ => {}
        }
    }
    None
}

/// A web address written out in running text, which GitHub turns into a link
/// without any markup around it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BareUrl {
    /// Byte range in the text that was searched.
    pub range: Range<usize>,
    /// What the link opens: the text itself, with `http://` in front of an
    /// address that starts with `www.`.
    pub url: String,
}

/// Find `http://`, `https://` and `www.` addresses, and e-mail addresses, in
/// plain text. An e-mail address links to `mailto:` it.
///
/// This follows GitHub's extended autolinks, with one difference: an address
/// stops at the first character outside ASCII. In Chinese or Japanese text an
/// address is usually followed directly by more text, with no space, and
/// GitHub's rule would pull that text into the link.
pub fn find_bare_urls(text: &str) -> Vec<BareUrl> {
    let mut found = find_web_addresses(text);
    for email in find_emails(text) {
        let overlaps = found
            .iter()
            .any(|url| url.range.start < email.range.end && email.range.start < url.range.end);
        if !overlaps {
            found.push(email);
        }
    }
    found.sort_by_key(|found| found.range.start);
    found
}

/// E-mail addresses as GitHub finds them: letters, digits and `.+-_` before
/// the `@`, a domain with at least one dot after it, ending in a letter or
/// digit.
fn find_emails(text: &str) -> Vec<BareUrl> {
    let bytes = text.as_bytes();
    let local = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-' | b'_');
    let domain = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_');
    let mut found = Vec::new();
    let mut search = 0;
    while let Some(at) = text[search..].find('@').map(|at| at + search) {
        search = at + 1;
        let mut start = at;
        while start > 0 && local(bytes[start - 1]) {
            start -= 1;
        }
        let mut end = at + 1;
        while end < bytes.len() && domain(bytes[end]) {
            end += 1;
        }
        // A full stop or other mark after the address ends the sentence.
        while end > at + 1 && !bytes[end - 1].is_ascii_alphanumeric() {
            end -= 1;
        }
        let host = &text[at + 1..end];
        let valid = start < at
            && host.contains('.')
            && !host.starts_with('.')
            && !host.contains("..")
            && bytes[at - 1] != b'.';
        if valid {
            let written = &text[start..end];
            found.push(BareUrl {
                range: start..end,
                url: format!("mailto:{written}"),
            });
            search = end;
        }
    }
    found
}

fn find_web_addresses(text: &str) -> Vec<BareUrl> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let Some(len) = url_prefix(&bytes[at..]) else {
            at += 1;
            continue;
        };
        // An address starts a word, or follows an opening delimiter.
        let starts_word = at == 0
            || matches!(
                bytes[at - 1],
                b' ' | b'\t' | b'\n' | b'\r' | b'*' | b'_' | b'~' | b'('
            )
            || !bytes[at - 1].is_ascii();
        if !starts_word {
            at += len;
            continue;
        }
        let mut end = at + len;
        while end < bytes.len() {
            let b = bytes[end];
            if !b.is_ascii() || b.is_ascii_whitespace() || b.is_ascii_control() || b == b'<' {
                break;
            }
            end += 1;
        }
        let end = (trim_url_end(&text[at..end]) + at).max(at + len);
        let domain = &text[at + len..end];
        let host = domain.split(['/', '?', '#']).next().unwrap_or("");
        let valid_host = !host.is_empty()
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
            && (host.contains('.') || !text[at..at + len].eq_ignore_ascii_case("www."));
        if valid_host && end > at + len {
            let written = &text[at..end];
            let url = if written[..4].eq_ignore_ascii_case("www.") {
                format!("http://{written}")
            } else {
                written.to_owned()
            };
            found.push(BareUrl {
                range: at..end,
                url,
            });
            at = end;
        } else {
            at += len;
        }
    }
    found
}

/// Length of the `http://`, `https://` or `www.` at the start of `bytes`.
fn url_prefix(bytes: &[u8]) -> Option<usize> {
    ["https://", "http://", "www."]
        .into_iter()
        .find(|prefix| {
            bytes.len() >= prefix.len()
                && bytes[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
        })
        .map(str::len)
}

/// Where an address really ends: trailing punctuation belongs to the
/// sentence, and a closing parenthesis only belongs to the address when it
/// has a matching opening one inside it.
fn trim_url_end(candidate: &str) -> usize {
    let mut end = candidate.len();
    loop {
        let Some(last) = candidate[..end].bytes().last() else {
            return end;
        };
        match last {
            b'?' | b'!' | b'.' | b',' | b':' | b'*' | b'_' | b'~' | b'\'' | b'"' | b';' => end -= 1,
            b')' => {
                let open = candidate[..end].bytes().filter(|b| *b == b'(').count();
                let close = candidate[..end].bytes().filter(|b| *b == b')').count();
                if close > open {
                    end -= 1;
                } else {
                    return end;
                }
            }
            _ => return end,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(text: &str) -> Vec<&str> {
        find_bare_urls(text)
            .into_iter()
            .map(|found| &text[found.range])
            .collect()
    }

    #[test]
    fn labels_compare_without_case_and_spacing() {
        assert_eq!(normalize_label("  Foo   Bar "), "foo bar");
        assert_eq!(normalize_label("中文 标签"), "中文 标签");
        assert_eq!(definition_label("[Foo]: /url"), Some("foo".into()));
        assert_eq!(definition_label("   [a b]: <x>"), Some("a b".into()));
        assert_eq!(definition_label("    [a]: /url"), None);
        assert_eq!(definition_label("[^1]: note"), None);
        assert_eq!(definition_label("[a] text"), None);
        assert_eq!(definition_label("[]: /url"), None);
        assert_eq!(definition_label(r"[a\]b]: /url"), Some(r"a\]b".into()));
    }

    #[test]
    fn bare_addresses_are_found_in_running_text() {
        assert_eq!(
            urls("see https://example.com/a?b=c and www.aster.dev."),
            ["https://example.com/a?b=c", "www.aster.dev"]
        );
        assert_eq!(urls("(http://example.com/x)"), ["http://example.com/x"]);
        assert_eq!(
            urls("http://en.wikipedia.org/wiki/Rust_(language)"),
            ["http://en.wikipedia.org/wiki/Rust_(language)"]
        );
        assert_eq!(
            find_bare_urls("www.example.com")[0].url,
            "http://www.example.com"
        );
    }

    #[test]
    fn an_address_stops_before_cjk_text() {
        let text = "请访问https://example.com/路径获取，或者 www.example.org。";
        assert_eq!(urls(text), ["https://example.com/", "www.example.org"]);
    }

    #[test]
    fn e_mail_addresses_link_to_mailto() {
        let text = "写信给 someone.name+tag@example.co.jp，或者a_b@mail.example.org。";
        let found = find_bare_urls(text);
        assert_eq!(
            urls(text),
            ["someone.name+tag@example.co.jp", "a_b@mail.example.org"]
        );
        assert_eq!(found[0].url, "mailto:someone.name+tag@example.co.jp");
        // Not addresses: no dot after the @, nothing before it, a doubled
        // dot, and an @ inside a web address.
        assert!(urls("@example.com").is_empty());
        assert!(urls("a@localhost").is_empty());
        assert!(urls("a@b..com").is_empty());
        assert_eq!(
            urls("https://example.com/@who/a@b.cn/x and a@b.cn"),
            ["https://example.com/@who/a@b.cn/x", "a@b.cn"]
        );
    }

    #[test]
    fn front_matter_at_the_start() {
        let text = "---\ntitle: 标题\n\ntags:\n  - a\n---\n# 正文\n";
        let range = front_matter(text).unwrap();
        assert_eq!(&text[range], "---\ntitle: 标题\n\ntags:\n  - a\n---\n");
        // Closed by `...`, with trailing spaces, at the end of the text.
        assert_eq!(front_matter("---\na: 1\n...  "), Some(0..14));
        assert_eq!(front_matter("---  \r\na: 1\r\n---\r\n"), Some(0..18));
    }

    #[test]
    fn things_that_are_not_front_matter() {
        for text in [
            "",
            "---",
            "---\n",
            // Never closed.
            "---\na: 1\n",
            // The first line is blank, or the end.
            "---\n\na: 1\n---\n",
            "---\n---\n",
            // Not at the very start, indented, or not exactly three.
            "\n---\na: 1\n---\n",
            " ---\na: 1\n---\n",
            "----\na: 1\n---\n",
            "---\na: 1\n----\n",
            "---\na: 1\n ---\n",
            "---x\na: 1\n---\n",
        ] {
            assert_eq!(front_matter(text), None, "{text:?}");
        }
        // Closed too far in.
        let long = format!("---\n{}\n---\n", "a".repeat(FRONT_MATTER_LIMIT));
        assert_eq!(front_matter(&long), None);
    }

    /// Why the parser option is not in `options()`: it takes a block in the
    /// middle of a document too.
    #[test]
    fn the_parser_takes_metadata_anywhere() {
        let text = "段落\n\n---\na: 1\n---\n";
        let options = options() | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS;
        assert!(
            Parser::new_ext(text, options)
                .any(|event| matches!(event, Event::Start(Tag::MetadataBlock(_))))
        );
        assert_eq!(front_matter(text), None);
    }

    #[test]
    fn words_for_a_double_click() {
        fn word<'a>(text: &'a str, at: &str) -> &'a str {
            let offset = text.find(at).unwrap();
            &text[word_range(text, offset)]
        }
        let text = "Hello, wide world";
        assert_eq!(word(text, "ello"), "Hello");
        assert_eq!(word(text, "wide"), "wide");
        assert_eq!(word(text, ","), ",");
        assert_eq!(word(text, " wide"), " ");
        assert_eq!(&text[word_range(text, text.len())], "world");
        assert_eq!(&text[word_range(text, 1000)], "world");
        // Han characters split into dictionary words, not single characters
        // or the whole run.
        let text = "我们今天学习中文";
        let found = word(text, "学");
        assert!(
            found.len() > "学".len() && found.len() < text.len(),
            "{found}"
        );
        assert!(found.starts_with('学'), "{found}");
        // Japanese: kana and kanji.
        let text = "日本語の文章を選択する";
        let found = word(text, "文");
        assert!(found.starts_with('文') && !found.contains('を'), "{found}");
        // Inside a character: the character it is in.
        assert_eq!(word_range("中文", 1), 0..6);
        assert_eq!(word_range("", 0), 0..0);
    }

    #[test]
    fn things_that_are_not_addresses() {
        assert!(urls("ahttp://example.com").is_empty());
        assert!(urls("http://").is_empty());
        assert!(urls("www.").is_empty());
        assert!(urls("wwwexample").is_empty());
        assert!(urls("").is_empty());
    }
}
